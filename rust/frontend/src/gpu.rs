// gpu.rs — winit + wgpu surface lifecycle.
//
// Owns the winit Window, wgpu Surface/Device/Queue, and the text layer
// (text.rs). Drives a redraw on RedrawRequested: clears, then draws text on
// top in the same render pass.
//
// chrome.rs (ratatui custom Backend) and preview.rs (preview-layer surface)
// will plug in here as additional draw stages, both feeding into the same
// wgpu surface — see ADR 0011 for the chrome-vs-preview-layer split.

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
use crate::keybindings::{Action, KeyBindings};
use crate::preview::markdown::{
    FigureMetrics, FigureMetricsMap, MarkdownPreview, MathMetrics, MathMetricsMap,
    BODY_SIZE as MD_BODY_SIZE,
};
use crate::preview::png::quad_from_png_bytes;
use crate::preview::quad::{Quad, QuadPipeline, ScreenRect};
use crate::preview::svg::quad_from_svg_bytes;
use crate::settings::Settings;
use crate::transport::OutgoingReq;
use sot_protocol::{ReplFrame, TreeNode};

/// Which root tree the left pane is showing. Files mode → backend's files
/// hierarchy via `tree.root {mode: "files"}`; Modules mode → kernel's loaded
/// module list via `kernel.request modules.list`; Sessions mode → backend
/// tmux registry (ADR 0013) via `tmux.list_sessions`. Cursor position is
/// *not* preserved across switches in the spike — switching re-roots the
/// tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Files,
    Modules,
    Sessions,
    /// ADR 0015 — host registry picker. Lists entries from
    /// `hosts.toml`; Enter persists `last_host` to state-toml so the
    /// next launcher run targets the chosen host. Doesn't change the
    /// live transport — switching hosts means quit + relaunch.
    Hosts,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Mode::Files => "files",
            Mode::Modules => "modules",
            Mode::Sessions => "sessions",
            Mode::Hosts => "hosts",
        }
    }
}

/// Which of the four quadrant panes has keyboard focus. Spatial moves
/// via Ctrl+Arrow. Tab is deliberately not a focus switcher — it must
/// reach the terminal panes for shell/REPL completion. Status-line and
/// the panel borders signal which is active.
///
/// Pane semantics:
///   - NavTree consumes character keys as mode/nav shortcuts.
///   - Repl consumes character keys as code to evaluate.
///   - Preview / Llm are passive today (focused for visual indication and
///     so Ctrl+Arrow has a four-corner home); scroll/interaction lands
///     when those panes grow real affordances.
///
/// User-configurable layout + keybindings live in a settings file the LLM
/// can edit — TODO. For now the names + Ctrl+Arrow adjacency are coded
/// directly so the structure stays obvious.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    NavTree,
    Preview,
    Llm,
    Repl,
}

impl PaneFocus {
    /// Spatial neighbour of `self` in `dir`. Updated for the 3-column
    /// layout (ADR 0014): nav | preview | llm horizontally, REPL below
    /// any column when the drawer is open. Movement off the layout (or
    /// into a hidden drawer) returns `self` — the caller handles the
    /// drawer-closed skip-Repl rule in its Ctrl+Arrow path.
    fn move_in(self, dir: SpatialDir) -> Self {
        use PaneFocus::*;
        use SpatialDir::*;
        match (self, dir) {
            // Horizontal walk through the column row.
            (NavTree, Right) => Preview,
            (Preview, Right) => Llm,
            (Llm, Left) => Preview,
            (Preview, Left) => NavTree,
            // Drawer entry from any column row.
            (NavTree, Down) | (Preview, Down) | (Llm, Down) => Repl,
            // Drawer exit maps to the column above the direction pressed:
            // the drawer spans all three columns, so Left -> nav, Up ->
            // preview (middle, closest to the eyes), Right -> llm.
            (Repl, Up) => Preview,
            (Repl, Left) => NavTree,
            (Repl, Right) => Llm,
            _ => self,
        }
    }
}

/// What the bottom drawer is showing. `Closed` = hidden (three columns
/// occupy the full window height); `Repl` = the Julia REPL (Ctrl+J);
/// `Terminal` = the local PTY terminal (Ctrl+T). `layout::compute` only
/// cares whether the drawer is open (`!= Closed`); the variant selects
/// which content renders into the drawer rect and the title label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DrawerContent {
    Closed,
    Repl,
    Terminal,
    Monitor,
}

impl DrawerContent {
    /// True when the drawer occupies vertical space (anything but `Closed`).
    fn is_open(self) -> bool {
        self != DrawerContent::Closed
    }

    /// Toggle `slot` per the symmetric Ctrl+J / Ctrl+T rule: pressing a
    /// drawer key opens its content, swaps to it if the other is showing,
    /// and closes if its own content is already showing.
    fn toggle(self, slot: DrawerContent) -> DrawerContent {
        if self == slot {
            DrawerContent::Closed
        } else {
            slot
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SpatialDir {
    Up,
    Down,
    Left,
    Right,
}

/// One submitted REPL eval — code typed by the user plus the frames the
/// kernel returned. `in_flight` means we sent the request but haven't seen
/// the response yet; the chrome renders an italicised `(running…)` until
/// the matching `ReplEvalDone` lands.
#[derive(Clone)]
struct ReplEntry {
    eval_id: u64,
    code: String,
    frames: Vec<ReplFrame>,
    elapsed_ms: u64,
    in_flight: bool,
    /// Which prompt the user was on when this entry was submitted —
    /// `false` = `julia>`, `true` = `pkg>`. Stored per-entry so a
    /// later mode switch doesn't relabel old scrollback rows.
    pkg_mode: bool,
}

/// One cached markdown-figure image. `natural_w_px / natural_h_px`
/// are the source bitmap's dimensions (before any preview-pane
/// downscale), used by the walk to size the FFFC placeholder line so
/// the figure paints at its native aspect inside the row. `quad` is
/// the GPU-side texture; cached so navigation doesn't re-upload.
struct FigureCacheEntry {
    quad: Quad,
    natural_w_px: u32,
    natural_h_px: u32,
}

/// One per-table cosmic-text buffer hosted as an ExtraArea. `rendered`
/// is the source text the buffer was last built from; on every redraw
/// we compare it against the matching MediaBlock::Table so navigating
/// to a doc with the same table count but different content still
/// rebuilds. `natural_w_px` is the measured widest LayoutRun, used by
/// the scroll clamp to keep the user from scrolling past the table's
/// right edge.
struct TableBufferEntry {
    rendered: String,
    buffer: cosmic_text::Buffer,
    natural_w_px: f32,
}

/// One cached MathJax-rendered math span. SVG bytes survive across
/// re-renders so navigation/scroll doesn't re-roundtrip; the
/// rasterised quad is built lazily on first paint and held in the
/// `rasterised` slot so subsequent frames skip the SVG decode.
struct MathSvg {
    svg_bytes: Vec<u8>,
    /// The `ex` pixel value the sidecar told MathJax to use when
    /// emitting this SVG (see `rust/backend/sidecars/mathjax/render.mjs`
    /// — currently hardcoded to 8). Combined with [`MATHJAX_EX_FACTOR`]
    /// it's how we recover the SVG's intended size relative to body
    /// text. Held but not directly consumed; [`width_ex`/`height_ex`]
    /// below carry the per-block geometry.
    #[allow(dead_code)]
    ex: f32,
    /// SVG root `width="N ex"` parsed at insert time. `None` if the
    /// `<svg>` tag couldn't be parsed (defensive — falls back to the
    /// pre-fix letterbox sizing path).
    width_ex: Option<f32>,
    /// SVG root `height="N ex"` — same parse path as `width_ex`.
    height_ex: Option<f32>,
    /// SVG root `style="vertical-align: N ex"`. Negative (the SVG
    /// hangs `N ex` below the text baseline). Reserved for the A4
    /// inline-placement pass; display blocks paint centred and don't
    /// consume this yet.
    #[allow(dead_code)]
    vertical_align_ex: Option<f32>,
    /// Lazily-rasterised quad. Populated on first paint inside the
    /// markdown pane; reset to None when the cache entry is replaced
    /// (response superseded by a re-render) so the next frame
    /// re-rasterises at the current pixel size.
    rasterised: Option<Quad>,
}

/// MathJax's `ex_factor` — the ratio of x-height to em-height for its
/// bundled TeX font. SVG dimensions come back in `ex` units; pixel size
/// at body font `F` (px) is `value_ex * MATHJAX_EX_FACTOR * F`.
/// MathJax-src's `CommonOutputJax.OPTIONS` defaults this to 0.5, and
/// that's what we bake in: the sidecar uses MathJax defaults
/// (`em: 16, ex: 8`, exactly 0.5), and any per-body-font override is a
/// future refinement once we measure x-height from fontdb.
const MATHJAX_EX_FACTOR: f32 = 0.5;

/// Pull `width="N ex"`, `height="N ex"`, and `style="vertical-align: N ex"`
/// out of the SVG's root tag. MathJax-SVG emits these as ASCII float
/// literals with a literal `ex` suffix, so a regex-free scan is enough.
/// Returns `None` for any attribute that isn't present or doesn't parse,
/// leaving the caller to fall back to letterbox sizing.
fn parse_math_svg_dims(svg_bytes: &[u8]) -> (Option<f32>, Option<f32>, Option<f32>) {
    let Ok(text) = std::str::from_utf8(svg_bytes) else {
        return (None, None, None);
    };
    // Limit the scan to the root opening tag — MathJax SVGs are huge
    // (one path per glyph), and the dims we want are always on the first
    // `<svg ...>` tag.
    let tag = match text.find("<svg") {
        Some(start) => {
            let end = text[start..]
                .find('>')
                .map(|n| start + n + 1)
                .unwrap_or(text.len());
            &text[start..end]
        }
        None => return (None, None, None),
    };
    let parse_ex_attr = |attr: &str| -> Option<f32> {
        // attr is something like `width="`. After the `=` we look for
        // `"`-delimited content ending in `ex`.
        let needle = format!("{attr}=\"");
        let i = tag.find(&needle)?;
        let rest = &tag[i + needle.len()..];
        let j = rest.find('"')?;
        let val = &rest[..j];
        let val = val.trim();
        let val = val.strip_suffix("ex")?.trim();
        val.parse::<f32>().ok()
    };
    let w = parse_ex_attr("width");
    let h = parse_ex_attr("height");
    // vertical-align lives inside the `style="..."` attribute, not as
    // its own attribute. Pull it out separately.
    let v = (|| -> Option<f32> {
        let i = tag.find("style=\"")?;
        let rest = &tag[i + "style=\"".len()..];
        let j = rest.find('"')?;
        let style = &rest[..j];
        let k = style.find("vertical-align:")?;
        let after = &style[k + "vertical-align:".len()..];
        let after = after.trim_start();
        // Strip up to the next `;`, end-of-style, or whitespace.
        let stop = after.find([';', ' ']).unwrap_or(after.len());
        let v = after[..stop].trim();
        let v = v.strip_suffix("ex")?.trim();
        v.parse::<f32>().ok()
    })();
    (w, h, v)
}

/// One workspace's snapshot of the chrome's view state, captured on
/// workspace switch and restored when the user comes back. Goal: the
/// frame after a swap-in looks identical to the frame before the swap-
/// out, modulo events that have arrived in the interim.
///
/// Captures *everything* mode-bearing about the chrome: the nav tree
/// (cursor + expanded folders), which mode it was rendering, scroll +
/// focus, the cached preview source (so the rendered preview repaints
/// without a backend round-trip), the concept-annotation slot, drift
/// badge bookkeeping, and the Sessions-mode pane-capture dedup memo.
///
/// REPL pane state (scrollback / input / history) lives in a sibling
/// snapshot type so the per-workspace eval routing in [`State`] can
/// land replies for non-active workspaces directly.
///
/// Held in `State.workspace_ui_snapshots`, keyed by workspace slug
/// (with `<default>` for the daemon-default workspace).
#[derive(Clone)]
struct WorkspaceUiSnapshot {
    /// Last mode the workspace was rendering. Dropped-then-restored so a
    /// workspace left in Modules mode comes back in Modules mode, not
    /// forced into Files.
    mode: Mode,
    /// Flattened nav tree at the moment of swap — cursor + expanded
    /// folders + per-row state. TreeView is Clone so this is a deep copy
    /// of the visible structure; re-fetch only happens if no snapshot
    /// exists yet for the entering workspace.
    tree: TreeView,
    /// Persistent nav-pane scroll offset (vim-style scrolloff). Cursor
    /// alone doesn't pin the viewport; direction-of-motion does, so
    /// swap-in needs both the cursor (in `tree`) and the offset.
    tree_scroll: u16,
    /// Tmux session the BL pane was attached to (`sot-be-<slug>`).
    /// Restored via `attach_session_to_bl` on swap-in.
    bl_pane_target: Option<String>,
    /// The node id we most recently asked the backend to preview, so
    /// switching back doesn't bounce-fetch the same preview again.
    preview_node_id_fired: Option<String>,
    /// C2 pin-and-leave state for this workspace. `Some` if a node was
    /// pinned when the user swapped away; restored on swap-in so the
    /// preview stays parked on the same file across workspace switches.
    pinned_preview_node_id: Option<String>,
    /// Last preview source (mime + raw bytes) the chrome rendered. On
    /// swap-in we feed this back through `render_preview_source` to
    /// rebuild the preview pane without a fresh `preview.get`.
    preview_src: Option<(String, Vec<u8>)>,
    /// Concept-annotation backing data, including `synced_against`
    /// for the drift badge. The *shaped* MarkdownPreview is not in the
    /// snapshot (cosmic-text Buffer isn't Clone-able); preview_concept
    /// is cleared on swap-in and the cursor-tracking
    /// `maybe_fire_concept_read` re-shapes it on the next frame from
    /// the fresh wire reply. The brief no-concept frame is the cost.
    concept: Option<ConceptInfo>,
    /// Drift-badge bookkeeping — paths whose AST hash we know, and
    /// paths we've already asked `file.parse` for. Per-workspace so
    /// hashes from workspace A don't leak into workspace B's tree.
    file_ast_hashes: std::collections::HashMap<String, String>,
    file_parse_fired: std::collections::HashSet<String>,
    /// Sessions-mode dedup memo — which pane we last fired
    /// `tmux.capture_pane` for so swap-back doesn't refire.
    tmux_capture_fired_for: Option<String>,
    /// Concept-write modal state (header / buffer / dirty flag /
    /// banners). Captured so swap-back returns the user to mid-edit
    /// without losing typed content. preview_edit is *not* in the
    /// snapshot — it gets re-shaped by `rebuild_edit_preview` from
    /// edit_state on restore.
    edit_state: Option<EditState>,
}

/// Per-workspace REPL pane state. Captured at swap-out and restored at
/// swap-in alongside [`WorkspaceUiSnapshot`]. Lives in its own type so
/// reply routing (`ReplEvalDone` for non-active workspaces) can mutate
/// just this slice without touching general UI state.
///
/// Each workspace's REPL runs on its own kernel child (ADR 0014), so
/// the eval counter is naturally per-workspace too — when we route
/// replies back to the right log we keep the counter and the log in
/// sync.
#[derive(Clone)]
struct WorkspaceReplSnapshot {
    /// Submitted evals + the kernel's reply frames. Bounded the same
    /// way the live log is (last 256 entries) when captured.
    repl_log: Vec<ReplEntry>,
    /// Mid-typed input at swap time. Restored verbatim on swap-in so
    /// the user can keep editing whatever they were composing.
    repl_input: String,
    /// Per-workspace eval id counter. Backend doesn't require these
    /// to be globally unique; matching `eval_id → entry` works the
    /// same on every workspace.
    repl_eval_counter: u64,
    /// `]`/Backspace prompt-mode toggle (julia> vs pkg>) is per-
    /// workspace too — switching to a workspace mid-pkg-shell returns
    /// you to pkg>.
    repl_pkg_mode: bool,
    /// Scrollback offset captured at swap-out.
    repl_scroll: u16,
    /// History-walk state. `Some` means the workspace was in the
    /// middle of an Up/Down history walk; restoring puts the user
    /// back exactly where they were.
    history_pos: Option<usize>,
    history_saved: Option<String>,
}

/// Sessions-mode workspace picker (ADR 0014). When `State.workspace_picker`
/// is `Some(this)`, the NavTree renders this directory listing instead of
/// the Sessions list. Up/Down moves the cursor; Right drills into a
/// subdirectory (refires `directory.list`); Left ascends to the parent; Enter
/// commits the cursored directory as the new workspace's project_root (with the
/// ccb agent), Shift+Enter commits it as a bare session (no LLM agent); Esc
/// cancels. Commit chords are keymap-driven (session.create / .create_bare).
struct WorkspacePicker {
    /// Absolute path of the directory we're currently showing. The
    /// title bar in the NavTree displays this so the user always knows
    /// where they are.
    current_path: String,
    /// Subdirectory rows under `current_path`. Populated by the
    /// `IncomingEvt::DirectoryList` handler when the path echoes ours.
    entries: Vec<crate::transport::DirEntry>,
    /// Cursor into `entries`. `0`-based; clamped on each refresh.
    selected: usize,
}

/// A one-line modal prompt that floats over the NavTree and steals
/// keystrokes while it's `Some` (mirrors how `WorkspacePicker` and
/// `EditState` intercept keys). Each variant carries the context the
/// confirm path needs. The enum is the extension point: new nav-pane
/// modals add a variant here and a match arm in the key handler +
/// renderer, without touching the surrounding nav code.
#[derive(Clone)]
enum NavPrompt {
    /// Ctrl+N in Files mode: type the name of a new file to create in
    /// `dir_node_id`. Enter confirms (validates + fires `file.write`
    /// with empty content), Esc cancels. `input` is the live name buffer.
    CreateFile {
        /// `files:`-prefixed id of the directory that will contain the
        /// new file (`files:` for the project root). The new file's id is
        /// `build_new_file_node_id(dir_node_id, input)`.
        dir_node_id: String,
        /// Live name buffer, rendered after `new file: ` on the status line.
        input: String,
    },
    /// Ctrl+D in Files mode: confirm trashing the cursored file. `y`/`Y`
    /// fires `file.delete` for `node_id`; `n`/`N`/Esc/any other key cancels.
    /// No text input — it's a y/N gate. `label` is the file's display name,
    /// shown in the `delete <label>? [y/N]` status line.
    ConfirmDelete {
        /// `files:`-prefixed id of the file to delete.
        node_id: String,
        /// Display label of the row, echoed in the confirm prompt.
        label: String,
    },
}

/// Active concept-annotation edit. `None` when the preview pane is in
/// read-only view mode (the default); `Some` when the user pressed `e`
/// on a cursored annotation. Carries enough context to fire a
/// well-formed `concept.write` and to discriminate a stale-write
/// refusal from a fresh-edit refusal.
#[derive(Clone)]
struct EditState {
    /// Concept target being edited (`files/path/to.rs`,
    /// `modules/Foo`, ...). Matched against `ConceptWriteDone.target`
    /// when the write reply lands.
    target: String,
    /// `synced_against` AST hash captured at edit-enter time. Sent as
    /// `expected_ast_hash` on every save so the backend gates the
    /// write with optimistic concurrency (Linux's `4ebca35`).
    expected_ast_hash: Option<String>,
    /// YAML frontmatter (`---\n...\n---\n`) captured at edit-enter,
    /// or `None` when the source had none. Rendered above the
    /// editable region as a read-only header; concatenated with
    /// `buf.body()` on save so the on-disk file's frontmatter
    /// survives round-trip byte-perfect.
    header: Option<String>,
    /// Body captured when entering edit mode — used to detect whether
    /// the buffer is dirty (the discard-confirm modal cares; save
    /// snaps this to the current body so post-save edits start
    /// clean again).
    original: String,
    /// Live editable text + cursor. Holds *body only* — frontmatter
    /// is in `header`.
    buf: EditBuffer,
    /// True while the discard-confirm modal is up (Esc pressed on a
    /// dirty buffer). `y` confirms discard, `n` / Esc / any other key
    /// dismisses and returns to editing.
    confirm_discard: bool,
    /// True while the stale-write banner is up (a `concept.write`
    /// returned `stale_write` because the on-disk `synced_against`
    /// no longer matches our `expected_ast_hash`). `r` re-reads the
    /// file and replaces the buffer with the on-disk content
    /// (discarding edits); `k` dismisses the banner and lets the
    /// user keep editing (the next save will fail again until they
    /// reload or the file changes back). Never auto-clobber.
    stale_banner: bool,
    /// `Some(files:<relpath>)` when editing a general source file (vs a
    /// `.concept/` annotation, where this is `None`). Selects the save path:
    /// `Some` → `file.write` keyed on this node id; `None` → `concept.write`
    /// keyed on `target`.
    file_node_id: Option<String>,
    /// Content version from the file's `file.read`, sent back as
    /// `file.write`'s `expected_version` for optimistic concurrency. `None`
    /// for concept edits (they gate on `expected_ast_hash`).
    file_version: Option<String>,
}

impl EditState {
    fn is_dirty(&self) -> bool {
        self.buf.body() != self.original
    }

    /// Reassemble the full on-disk content from `header` + the live
    /// buffer body — sent as the payload of every `concept.write`.
    fn full_content(&self) -> String {
        match &self.header {
            Some(h) => h.clone() + self.buf.body(),
            None => self.buf.body().to_string(),
        }
    }
}

/// Annotation read for one node. Cached on `State`; the chrome reads it to
/// decide whether to show "annotation: present" or "(no annotation)".
#[derive(Clone)]
struct ConceptInfo {
    target: String,
    exists: bool,
    #[allow(dead_code)] // body markdown is shaped into preview_concept on arrival
    content: String,
    /// `synced_against` AST hash parsed out of the annotation's YAML
    /// frontmatter. `None` when no annotation exists, no frontmatter, or
    /// the field is absent. Compared against `file_ast_hashes[path]` to
    /// drive the drift badge.
    synced_against: Option<String>,
}

/// Decode the bytes from a `figure.get` reply into a FigureCacheEntry
/// — a GPU quad sized to the source bitmap's natural dimensions plus
/// the dimensions themselves (so the markdown walk can reserve the
/// right placeholder height on the next reflow). Routes raster mimes
/// through the `image` crate and SVG through resvg's pipeline; any
/// other mime is rejected so an unknown response doesn't silently
/// upload garbage.
fn decode_figure_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &crate::preview::quad::QuadPipeline,
    mime: &str,
    bytes: &[u8],
) -> anyhow::Result<FigureCacheEntry> {
    if mime == "image/svg+xml" {
        // Rasterise at a reasonable natural size — SVG has no
        // intrinsic pixel dimensions, so we pick something sensible
        // and the paint pass downscales to fit the preview pane.
        // 800×600 is plenty for figures in markdown; tighter than the
        // 2k×2k that resvg would happily render if we let it scale to
        // an arbitrary target.
        let w: u32 = 800;
        let h: u32 = 600;
        let quad = crate::preview::svg::quad_from_svg_bytes(device, queue, pipeline, bytes, w, h)?;
        Ok(FigureCacheEntry {
            quad,
            natural_w_px: w,
            natural_h_px: h,
        })
    } else if mime.starts_with("image/") {
        let (quad, w, h) =
            crate::preview::png::quad_and_dims_from_bytes(device, queue, pipeline, bytes)?;
        Ok(FigureCacheEntry {
            quad,
            natural_w_px: w,
            natural_h_px: h,
        })
    } else {
        anyhow::bail!("unsupported figure mime: {mime}");
    }
}

/// Resolve a markdown image URL against the current markdown file's
/// node id to produce a files-mode node id the backend can fetch.
///
/// Inputs:
/// - `md_node_id`: `Some("files:examples/preview/foo.md")` when a md
///   file is open, `None` otherwise. We need this to know the
///   directory the relative url is anchored to.
/// - `url`: whatever was inside the `![](…)` parens.
///
/// Returns `None` for URLs we can't fetch via `files:` node ids:
///   - remote schemes (`http://`, `https://`, `file://`, etc.)
///   - URLs that walk out of the project root via `..`
///   - any url when `md_node_id` is missing
///
/// Forward slashes are canonical on the wire — files_mode's
/// node_id_to_path splits on both, but generating slashes here keeps
/// captures comparable across Linux + Windows.
fn resolve_figure_node_id(md_node_id: &Option<String>, url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    // Remote / data URLs — we don't fetch over network.
    if url.contains("://") || url.starts_with("data:") {
        return None;
    }
    let md = md_node_id.as_ref()?;
    let md_rel = md.strip_prefix("files:")?;
    // Parent dir of the markdown file. `foo.md` → `""`, `docs/foo.md` →
    // `docs`. We split on both separators to stay defensive even though
    // node_ids should already use `/`.
    let parent: String = match md_rel.rsplit_once(['/', '\\']) {
        Some((p, _)) => p.replace('\\', "/"),
        None => String::new(),
    };
    // Absolute (project-rooted) urls like `/figures/foo.png` map to
    // `files:figures/foo.png` — drop the leading slash and treat the
    // remainder as a root-relative path. Otherwise it's relative to the
    // markdown file's parent.
    let combined = if let Some(rest) = url.strip_prefix('/') {
        rest.to_string()
    } else if parent.is_empty() {
        url.to_string()
    } else {
        format!("{parent}/{url}")
    };
    // Normalise `.` / `..` segments. `..` walking past the root rejects
    // — files_mode would reject it on the backend anyway, but failing
    // here saves the round-trip.
    let mut stack: Vec<&str> = Vec::new();
    for seg in combined.split(['/', '\\']) {
        match seg {
            "" | "." => continue,
            ".." => {
                if stack.pop().is_none() {
                    return None;
                }
            }
            other => stack.push(other),
        }
    }
    if stack.is_empty() {
        return None;
    }
    Some(format!("files:{}", stack.join("/")))
}

/// Pull `synced_against: <value>` out of a markdown file's leading YAML
/// frontmatter. Accepts quoted (`"x"` / `'x'`) and bare values; trims
/// whitespace. Returns `None` when no frontmatter, no closing fence, or
/// the field isn't present. Matches the minimal parser Linux used on the
/// kernel side (`6864c93`) — full YAML is overkill here.
fn parse_synced_against(s: &str) -> Option<String> {
    let mut lines = s.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            return None;
        }
        let Some(rest) = trimmed.strip_prefix("synced_against:") else {
            continue;
        };
        let v = rest.trim();
        let unquoted = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        let unquoted = unquoted.trim();
        return if unquoted.is_empty() {
            None
        } else {
            Some(unquoted.to_string())
        };
    }
    None
}

/// Strip a leading YAML frontmatter block from an annotation. Matches the
/// jekyll-style `---\n…\n---\n` envelope; returns the original string
/// unchanged when no opening delimiter is on line 1. Tolerant of `\r\n`
/// line endings via `str::lines`.
fn strip_frontmatter(s: &str) -> String {
    split_frontmatter(s).1
}

/// Split a concept-file source into `(header, body)` where `header` is
/// the YAML frontmatter (including both `---` delimiters and the
/// trailing newline) or `None` when there is no frontmatter. The
/// concatenation `header.unwrap_or_default() + body` reproduces a
/// frontmatter-free file exactly and a frontmatter-bearing file
/// modulo a possibly-missing trailing newline after the closing
/// `---` (always preserved here when present in the input).
///
/// Edit mode uses this so the editable buffer is the body only —
/// frontmatter (target, target_kind, synced_against, authored_by,
/// references) renders as a read-only header above the edit area and
/// concatenation on save preserves it byte-perfect.
/// Hand `url` (any browser-openable address — `http://…`, `file:///…`, or
/// a local filesystem path) off to the OS default handler. Fire-and-
/// forget — we don't wait for the browser to exit.
fn open_url_in_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Avoid `cmd /c start`: shell metacharacters in URLs, especially
        // `&secret=...` on Pluto links, are otherwise parsed by cmd.exe.
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())?;
    }
    tracing::info!(%url, "opened in browser");
    Ok(())
}

/// Write `html_bytes` to a unique temp file and hand it off to the OS
/// default browser via `open_url_in_browser`. We don't delete the temp
/// file (the OS cleans temp on its own schedule; a fresh path per call
/// also prevents the browser from showing a stale cached version).
fn open_html_in_browser(html_bytes: &[u8]) -> std::io::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut path = std::env::temp_dir();
    path.push(format!("sot-preview-{now}.html"));
    std::fs::write(&path, html_bytes)?;
    let path_str = path.to_string_lossy().to_string();
    open_url_in_browser(&path_str)
}

fn split_frontmatter(s: &str) -> (Option<String>, String) {
    let mut lines = s.split('\n');
    let Some(first) = lines.next() else {
        return (None, String::new());
    };
    if first.trim() != "---" {
        return (None, s.to_string());
    }
    let mut header_lines = vec![first.to_string()];
    let mut body_lines: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines {
        if !closed {
            header_lines.push(line.to_string());
            if line.trim() == "---" {
                closed = true;
            }
        } else {
            body_lines.push(line);
        }
    }
    if !closed {
        return (None, s.to_string());
    }
    // `split('\n')` on a string ending with `\n` produces a trailing
    // empty element; rejoining with `\n` reproduces the original.
    let header = header_lines.join("\n");
    // Add the newline that separates header from body (it was the
    // `\n` after the closing `---` in the source).
    let header = header + "\n";
    let body = body_lines.join("\n");
    (Some(header), body)
}

/// Derive a `.concept/`-relative target string for a tree node id. Returns
/// `None` when the node has no natural annotation target (root rows, unknown
/// id prefixes). The backend rejects `..` / absolute paths inside the
/// `target` so we keep this conservative — only the `files:` and `modules:`
/// prefixes today, both producing forward-slash paths.
/// Mirror of `backend/src/paths.rs::slug`. Kept here as a duplicate
/// because the path-derivation rule needs to live in both backend (to
/// pick its own socket from `--label`) and frontend (to pre-compute the
/// tmux session name shown in Sessions mode before the daemon is even
/// alive). Centralisation into the protocol crate is a phase-2.5 polish.
#[allow(dead_code)] // last consumer (label prompt) removed in workspace-picker
                    // commit; keep for the next user that needs the slug rule
                    // on the frontend side without dragging in the protocol
                    // crate
fn slug_for_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut last_dash = false;
    for ch in label.chars() {
        let c = ch.to_ascii_lowercase();
        let keep = c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-';
        if keep {
            out.push(c);
            last_dash = c == '-';
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

/// Cap on bytes the preview pane will shape as text. A binary blob (or a
/// multi-hundred-KB text file) shaped via cosmic-text/comrak on the single
/// render thread freezes every pane — this is the guard against e.g. opening
/// a multi-GB `.h5` whose backend fallback returns raw bytes. Above this,
/// or if the content looks binary, the pane shows a one-line summary instead.
const PREVIEW_TEXT_CAP: usize = 512 * 1024;

/// Heuristic: does this blob look like binary (not safe to shape as text)?
/// A NUL byte in the leading window is the classic giveaway — text files
/// don't contain `0x00`, but HDF5 / images / archives / most binaries do
/// near the start. Cheap: scans at most the first 8 KiB.
fn looks_binary(bytes: &[u8]) -> bool {
    let window = &bytes[..bytes.len().min(8192)];
    window.contains(&0)
}

/// Dark square-ish app logo, embedded at build time from the repo root.
/// Drawn miniature flanking each session badge in the bottom strip. Cosmetic
/// only — a decode failure leaves `State::logo_quad` None and the strip renders
/// exactly as before.
const LOGO_DARK_PNG: &[u8] = include_bytes!("../../../logo-dark.png");
/// Wide full-text "wordmark" logo, embedded at build time from the repo root.
/// Drawn small at the top-left of the nav pane. Cosmetic only — a decode
/// failure leaves `State::wordmark_quad` None.
const LOGO_WORDMARK_PNG: &[u8] = include_bytes!("../../../logo-wordmark-dark.png");

/// Max characters shown for one session label in the bottom strip before
/// truncating with an ellipsis — keeps a long workspace name from dominating.
const STRIP_MAX_LABEL: usize = 24;
/// Gap between adjacent session labels in the strip, in cell-widths.
const STRIP_GAP_CELLS: f32 = 3.0;
/// Easing time-constant (seconds) for the strip slide — ~50 ms → an
/// exponential ease-out that settles in ~150 ms, frame-rate independent.
const STRIP_TAU: f32 = 0.05;
/// Vertical lift (in cell-widths) applied to the ACTIVE session's name in the
/// bottom strip so it's distinguishable by POSITION, not just colour — it sits
/// raised above its peers like a selected tab. Subtle; tune to taste.
const STRIP_ACTIVE_LIFT_CELLS: f32 = 0.4;

/// Wheel-spin gimmick: cycling workspaces (`Shift+←/→` → `cycle_workspace`)
/// flicks the brand wheels that bookend the bottom session strip — forward
/// spins them clockwise, backward counter-clockwise — and they spin down. Each
/// cycle adds `WHEEL_FLICK_VEL` rad/s (signed by direction, clamped to
/// `WHEEL_MAX_VEL` so a burst of presses doesn't blur), decaying with time
/// constant `WHEEL_TAU`; below `WHEEL_MIN_VEL` the spin settles and the angle
/// just rests wherever it stopped (a wheel looks fine at any rotation).
const WHEEL_FLICK_VEL: f32 = 13.0;
const WHEEL_MAX_VEL: f32 = 42.0;
const WHEEL_TAU: f32 = 0.45;
const WHEEL_MIN_VEL: f32 = 0.05;

/// Truncate a session label to `STRIP_MAX_LABEL` chars, ellipsizing if longer.
fn strip_truncate(label: &str) -> String {
    let n = label.chars().count();
    if n <= STRIP_MAX_LABEL {
        label.to_string()
    } else {
        let mut s: String = label
            .chars()
            .take(STRIP_MAX_LABEL.saturating_sub(1))
            .collect();
        s.push('…');
        s
    }
}

/// Strip-local center-x (pixels) of the active item — the value
/// `strip_scroll_px` eases toward so the active session sits at screen
/// center. Items lay out left→right, each `len*cell_w` wide with
/// `STRIP_GAP_CELLS*cell_w` between them.
fn session_strip_target(labels: &[String], active: usize, cell_w: f32) -> f32 {
    let gap = STRIP_GAP_CELLS * cell_w;
    let mut cursor = 0.0;
    for (i, lab) in labels.iter().enumerate() {
        let w = lab.chars().count() as f32 * cell_w;
        if i == active {
            return cursor + w / 2.0;
        }
        cursor += w + gap;
    }
    0.0
}

/// Build the pixel-positioned `Line`s for the bottom session strip. The
/// active item is centered at `win_w/2` (via `scroll_px`) and bold; items
/// fully off the window are culled. `baseline_y` is the glyph-top y in
/// physical pixels.
///
/// `tones[i]` (parallel to `labels`, `None` past its end) colours each name by
/// its agent's work-state — working = green, waiting = yellow, blocked = red,
/// done = blue — so a
/// session that's running or waiting on you stands out *even when it isn't the
/// active one* (that's the at-a-glance value). Idle / no-agent names keep the
/// original styling (active = bright, others = dim), matching "idle = current
/// colour". Colours come from `AgentTone::rgb`, so the strip and the
/// Sessions-mode rows are pixel-identical. A wilted (stale) "working" dims.
///
/// `contrast_dim` is the `--contrast-mode` lever: under "bright" (false) the
/// active name pops by going brighter + bold; under "dim" (true) the
/// *non*-active names are faded so the active one pops by contrast. Applied
/// to every name (coloured or not), routed through `contrast_tone_rgb` for
/// the coloured ones so it matches the nav rows exactly.
///
/// `flashes[i]` (parallel to `labels`, `0.0` past its end) is the
/// status-change flash factor for that name — when `> 0` its colour is
/// lerped toward white (composed after the contrast lever), so a name whose
/// work-state just changed blinks bright then fades back.
///
/// `pendings[i]` (parallel to `labels`, `false` past its end) is the badge
/// floor (ADR 0025 §1) flag: a workspace with a pending nav.preview result
/// waiting. When set, the name gets a leading `●` sigil and bright white + bold
/// (overriding the tone/contrast colour) so it reads as "a result is
/// waiting here", distinct from the work-state colours. Non-disruptive — it
/// only changes how the name renders, never the view.
fn session_strip_lines(
    labels: &[String],
    active: usize,
    scroll_px: f32,
    win_w: f32,
    cell_w: f32,
    baseline_y: f32,
    tones: &[Option<(AgentTone, bool)>],
    contrast_dim: bool,
    flashes: &[f32],
    pendings: &[bool],
) -> Vec<crate::text::Line> {
    let gap = STRIP_GAP_CELLS * cell_w;
    let mut out = Vec::new();
    let mut cursor = 0.0;
    for (i, lab) in labels.iter().enumerate() {
        // Badge floor (ADR 0025 §1): prefix a `●` sigil to a name whose
        // workspace has a pending nav.preview result. Done before the width
        // math so the strip layout accounts for the extra glyph and names
        // don't overlap. The bright-white accent is applied to the colour below.
        let pending = pendings.get(i).copied().unwrap_or(false);
        let lab: String = if pending {
            format!("●{lab}")
        } else {
            lab.clone()
        };
        let lab = &lab;
        let w = lab.chars().count() as f32 * cell_w;
        let center = cursor + w / 2.0;
        let left = win_w / 2.0 + (center - scroll_px) - w / 2.0;
        cursor += w + gap;
        if left + w < 0.0 || left > win_w {
            continue; // fully off-screen
        }
        let is_active = i == active;
        let flash = flashes.get(i).copied().unwrap_or(0.0);
        // Working/waiting/blocked/done override the colour so they're visible
        // regardless of which session is active. Idle (and no agent state)
        // keep the pre-state-nav styling so "idle = current colour" holds.
        // The contrast lever composes on top via `contrast_tone_rgb` for the
        // coloured branch; for the plain branches the "dim" lever bakes an
        // explicitly-dimmed colour (text.rs's fixed 0.65 DIM can't be made
        // stronger through the `dim` flag alone). The flash is applied last.
        let (color, dim) = match tones.get(i).copied().flatten() {
            Some((
                tone @ (AgentTone::Working
                | AgentTone::Waiting
                | AgentTone::Blocked
                | AgentTone::Done),
                wilted,
            )) => {
                let (rgb, _bold, d) =
                    contrast_tone_rgb(tone, wilted, is_active, contrast_dim, flash);
                (rgb, d)
            }
            _ if is_active => (Some(flash_plain(flash, (250, 250, 215))), false),
            // Non-active idle / no-agent name. "dim" lever fades it harder
            // than the default DIM by baking the colour; "bright" keeps the
            // pre-state-nav dim styling.
            _ if contrast_dim => (
                Some(flash_plain(
                    flash,
                    scale_rgb((204, 204, 204), CONTRAST_DIM_FACTOR),
                )),
                false,
            ),
            // Plain non-active under "bright": keep the default DIM unless a
            // flash is live, in which case resolve the default fg + lerp it.
            _ if flash > 0.0 => (Some(lerp_to_white((204, 204, 204), flash)), false),
            _ => (None, true),
        };
        // Badge floor (ADR 0025 §1): a pending name overrides whatever tone /
        // contrast colour it would otherwise get with bright white + bold
        // (matching the nav-row badge), clearing the dim so the `●`-prefixed
        // name reads as "result waiting here" — distinct from the work-state
        // colours without adding another hue.
        let (color, dim) = if pending {
            (Some((255, 255, 255)), false)
        } else {
            (color, dim)
        };
        out.push(crate::text::Line {
            text: lab.clone(),
            x: left,
            // Lift the active name a bit so it pops by VERTICAL position, not
            // just colour (it was hard to tell the active session from the
            // bottom). Smaller y = higher; top-left origin.
            y: if is_active {
                baseline_y - STRIP_ACTIVE_LIFT_CELLS * cell_w
            } else {
                baseline_y
            },
            color,
            bold: is_active || pending,
            italic: false,
            dim,
        });
    }
    out
}

/// Status-line text for a badged (pending) nav.preview result (ADR 0025 §1).
/// Pure so the badge-floor entry point's user-facing string is unit-testable
/// without constructing a full `State`. Reads as "a result is ready for this
/// workspace; switch to it to view".
fn pending_nav_status(ws: &str, path: &str) -> String {
    format!("result ready · {ws} · {path} — switch to view")
}

/// Ancestor directory relpaths of a workspace-relative file path, deepest
/// first: `"a/b/c.jl"` → `["a/b", "a"]`. Empty for a root-level path (no
/// `/`). Drives the deep-path reveal's level-by-level expansion
/// (`drive_reveal_step` expands the deepest *visible* one each round-trip).
/// Pure so the ordering — which determines we expand from the bottom up — is
/// unit-testable without a full `State`.
fn ancestor_rels(rel: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut acc = rel;
    while let Some(pos) = acc.rfind('/') {
        acc = &acc[..pos];
        out.push(acc);
    }
    out
}

/// The `files:` node id of a file row's parent directory. `files:foo/bar.txt`
/// → `files:foo`; a root-level `files:bar.txt` → `files:` (the root). Non-
/// `files:` ids pass through unchanged. Used to refresh the upload target dir.
/// True when an `op::FE_COMMAND` preview's `workspace` arg names the workspace
/// the FE is currently viewing — so the preview renders in place instead of
/// badging (fix for the dropped same-ws branch). `workspace` empty / "default"
/// / "<default>" means the daemon-default workspace, whose slug is
/// `default_slug`; the active view is `active_id` falling back to `default_slug`.
/// Pure so the same-ws decision is unit-testable without a live daemon/FE.
fn preview_targets_active_ws(
    active_id: Option<&str>,
    default_slug: Option<&str>,
    workspace: &str,
) -> bool {
    let is_default = workspace.is_empty() || workspace == "default" || workspace == "<default>";
    let target = if is_default {
        default_slug
    } else {
        Some(workspace)
    };
    let current = active_id.or(default_slug);
    target.is_some() && target == current
}

fn parent_files_node_id(node_id: &str) -> String {
    match node_id.strip_prefix("files:") {
        Some(rel) => match rel.rsplit_once('/') {
            Some((parent, _)) => format!("files:{parent}"),
            None => "files:".to_string(),
        },
        None => node_id.to_string(),
    }
}

/// Build + validate the `files:` node id for a new file `name` created inside
/// the directory `dir_node_id`. The backend's `node_id_to_path` rejects
/// absolute ids and `..` segments, so the only safe child id is
/// `<dir_id>/<name>` with a bare name — hence the name must not contain a path
/// separator. The root dir id is `files:` (trailing colon, no segment), so we
/// suppress the joining `/` in that case: `files:` + `a.txt` → `files:a.txt`;
/// `files:sub` + `a.txt` → `files:sub/a.txt`.
///
/// Returns `Err(reason)` for an empty name or one containing `/` or `\`; the
/// caller surfaces the reason on the status line and keeps the prompt open.
/// Collision against an existing sibling is checked separately by the caller
/// (it needs the live tree rows); this helper is pure so it stays testable.
fn build_new_file_node_id(dir_node_id: &str, name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name is empty");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("name must not contain a path separator");
    }
    // `files:` already ends with the prefix's colon — no separator needed at
    // the root; any deeper dir id gets a `/` before the bare name.
    let sep = if dir_node_id.ends_with(':') { "" } else { "/" };
    Ok(format!("{dir_node_id}{sep}{name}"))
}

/// Whether a Files-mode tree node is a directory for delete-refusal purposes.
/// Mirrors the dir test used by upload/download/create: `kind == "dir"`, plus
/// the files root (`files:`, which has no segment) is itself a directory.
/// `file.delete` refuses directories in v1, so the FE pre-refuses them before
/// even opening the confirm prompt. Pure so the refusal is unit-testable.
fn is_directory_row(node: &TreeNode) -> bool {
    node.kind == "dir" || node.id == "files:"
}

/// Give up on a file's drift check after this many failed `file.parse`
/// attempts (initial fire + 2 retries). Retries are spaced by exponential
/// backoff (2s, 4s) — per review feedback, short enough that a capture
/// window converges, capped so a persistently-failing kernel gets exactly
/// two more chances per session, never a storm.
const FILE_PARSE_MAX_RETRIES: u32 = 3;

fn node_id_to_concept_target(id: &str) -> Option<String> {
    if let Some(path) = id.strip_prefix("files:") {
        if path.is_empty() {
            None
        } else {
            Some(format!("files/{path}"))
        }
    } else if let Some(name) = id.strip_prefix("modules:") {
        if name.is_empty() {
            None
        } else {
            Some(format!("modules/{name}"))
        }
    } else {
        None
    }
}

/// One visible row in the Files-mode tree. The flat-list layout is the chrome
/// abstraction: ratatui renders top-to-bottom, and indent + disclosure char
/// communicate depth and expansion state. Collapsing a row drops every
/// strictly-deeper row beneath it; re-expanding re-fetches via `tree.children`
/// rather than caching, since the spike has no staleness story yet.
#[derive(Clone)]
struct TreeRow {
    node: TreeNode,
    depth: usize,
    expanded: bool,
}

#[derive(Clone)]
struct TreeView {
    rows: Vec<TreeRow>,
    selected: usize,
}

impl TreeView {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
        }
    }

    /// Capture the node id of the currently-selected row, if any. Used by
    /// row-mutating ops to re-anchor the cursor by node id across a
    /// wipe-and-refill so a transport reconnect / project.scan / stale
    /// tree.children reply doesn't kick the user back to row 0.
    fn selected_node_id(&self) -> Option<String> {
        self.rows.get(self.selected).map(|r| r.node.id.clone())
    }

    /// Replace the entire flat row list at once. Used by ops that
    /// deliver a fully-nested tree in one shot (today: `project.scan`)
    /// so the chrome doesn't have to walk `set_root` + per-level
    /// `apply_children` calls. Preserves the cursor on its previous node
    /// when that node is still present in the new rows; falls back to 0
    /// when the previously-cursored node is gone.
    fn set_flat(&mut self, rows: Vec<TreeRow>) {
        let prev_id = self.selected_node_id();
        let prev_selected = self.selected;
        let prev_len = self.rows.len();
        self.rows = rows;
        self.selected = prev_id
            .as_deref()
            .and_then(|id| self.rows.iter().position(|r| r.node.id == id))
            .unwrap_or(0);
        tracing::info!(
            prev_id = ?prev_id,
            prev_selected,
            prev_len,
            new_selected = self.selected,
            new_len = self.rows.len(),
            "TreeView::set_flat"
        );
    }

    /// Re-seed the view from a `tree.root` reply. Preserves the cursor on
    /// its previous node when that node is still present under the new
    /// root; falls back to 0 when the previously-cursored node is gone
    /// (different root, deleted, etc.).
    fn set_root(&mut self, root: TreeNode, children: Vec<TreeNode>) {
        let prev_id = self.selected_node_id();
        let prev_selected = self.selected;
        let prev_len = self.rows.len();
        let root_expanded = !children.is_empty();
        let mut rows = vec![TreeRow {
            node: root,
            depth: 0,
            expanded: root_expanded,
        }];
        for c in children {
            rows.push(TreeRow {
                node: c,
                depth: 1,
                expanded: false,
            });
        }
        self.rows = rows;
        self.selected = prev_id
            .as_deref()
            .and_then(|id| self.rows.iter().position(|r| r.node.id == id))
            .unwrap_or(0);
        tracing::info!(
            prev_id = ?prev_id,
            prev_selected,
            prev_len,
            new_selected = self.selected,
            new_len = self.rows.len(),
            "TreeView::set_root"
        );
    }

    /// Splice the children of `parent_id` into the flat list, replacing any
    /// previously-shown children for that parent. Marks the parent expanded
    /// so the disclosure char flips and Left can collapse it again.
    /// Preserves the cursor on its previous node by node-id lookup: if the
    /// node survives (it was outside the spliced range, or it reappears as
    /// one of the new children) the cursor follows it to the new index; if
    /// it was inside the spliced-out subtree and not in the new children,
    /// the cursor falls back to the parent row so the user stays anchored
    /// to the surrounding context.
    fn apply_children(&mut self, parent_id: &str, children: Vec<TreeNode>) {
        let Some((pidx, pdepth)) = self.rows.iter().enumerate().find_map(|(i, r)| {
            if r.node.id == parent_id {
                Some((i, r.depth))
            } else {
                None
            }
        }) else {
            tracing::debug!(%parent_id, "tree.children reply for unknown parent — ignoring");
            return;
        };
        let prev_id = self.selected_node_id();
        let mut end = pidx + 1;
        while end < self.rows.len() && self.rows[end].depth > pdepth {
            end += 1;
        }
        self.rows.drain((pidx + 1)..end);
        let child_depth = pdepth + 1;
        for (i, c) in children.into_iter().enumerate() {
            self.rows.insert(
                pidx + 1 + i,
                TreeRow {
                    node: c,
                    depth: child_depth,
                    expanded: false,
                },
            );
        }
        self.rows[pidx].expanded = true;
        let prev_selected = self.selected;
        self.selected = prev_id
            .as_deref()
            .and_then(|id| self.rows.iter().position(|r| r.node.id == id))
            .unwrap_or(pidx);
        tracing::info!(
            parent_id,
            prev_id = ?prev_id,
            prev_selected,
            pidx,
            new_selected = self.selected,
            new_len = self.rows.len(),
            "TreeView::apply_children"
        );
    }

    fn move_down(&mut self) {
        if self.selected + 1 < self.rows.len() {
            self.selected += 1;
        }
    }

    fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    /// Try to collapse the currently-selected row in place. Returns true if
    /// anything happened, so callers can decide whether `Left` should fall
    /// through to "move to parent" instead.
    fn collapse_selected(&mut self) -> bool {
        let idx = self.selected;
        if idx >= self.rows.len() || !self.rows[idx].expanded {
            return false;
        }
        let depth = self.rows[idx].depth;
        let mut end = idx + 1;
        while end < self.rows.len() && self.rows[end].depth > depth {
            end += 1;
        }
        self.rows.drain((idx + 1)..end);
        self.rows[idx].expanded = false;
        true
    }

    /// Index of the row whose subtree contains the current selection, if any.
    /// O(n) walk back to the first row at a strictly-lesser depth.
    fn parent_of_selected(&self) -> Option<usize> {
        let idx = self.selected;
        if idx == 0 {
            return None;
        }
        let depth = self.rows[idx].depth;
        if depth == 0 {
            return None;
        }
        (0..idx).rev().find(|&i| self.rows[i].depth < depth)
    }
}

/// Test SVG used as a placeholder until kernel-driven previews land. Mimics
/// Offline-mode markdown placeholder. The real preview content comes from
/// the backend via `preview.get` once transport connects; this string is
/// only what the pane shows when `--socket` / `--tcp` aren't set.
const SAMPLE_MARKDOWN: &str = r##"## Offline

`sot` is running without a backend.

Pass `--socket <path>` (Unix socket / Windows named pipe) or
`--tcp <host:port>` to connect, or set `$SOT_SOCKET` /
`$SOT_TCP`. Use a planned split-launch setup for two-terminal runs.
"##;
use crate::text::TextLayer;

/// Base cell metrics in physical pixels at 1.0 scale. State multiplies these
/// by the effective scale (`cli.scale * window.scale_factor()`) at startup.
/// Monospace 14 px / 18 px line height yields roughly 8.4 advance for most
/// fonts; we round to 9 so cells align cleanly with integer pixel positions.
/// cosmic-text-derived metrics will replace these constants once the font
/// system is queried directly.
const BASE_CELL_W: f32 = 9.0;
const BASE_CELL_H: f32 = 18.0;
const BASE_CHROME_ORIGIN_X: f32 = 12.0;
const BASE_CHROME_ORIGIN_Y: f32 = 12.0;
/// Trigger frame for `--capture`. Big enough for the transport task to push
/// connect → tree.root → preview.get back to the GPU thread, since each event
/// schedules its own redraw. Tunable if reconnect grows slower.
const CAPTURE_FRAME: u32 = 30;

pub struct App {
    state: Option<State>,
    /// Inputs forwarded into State::new the first time the event loop hands
    /// us a window. Held on App rather than constructed inside resumed() so
    /// main.rs can decide whether transport runs.
    evt_rx: Option<std::sync::mpsc::Receiver<crate::transport::IncomingEvt>>,
    rt: Option<tokio::runtime::Runtime>,
    cli: crate::cli::Cli,
    evt_tx: Option<std::sync::mpsc::Sender<crate::transport::IncomingEvt>>,
    /// GPU thread holds the sender; transport task gets the receiver out of
    /// `req_rx` on first `resumed`. Optional because offline mode doesn't
    /// spawn transport at all, but the sender lives on State unconditionally
    /// so keybinding code doesn't need to special-case it.
    req_tx: tokio::sync::mpsc::UnboundedSender<crate::transport::OutgoingReq>,
    req_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::transport::OutgoingReq>>,
    /// Tracks Ctrl/Shift/Alt/Super state for Ctrl+Arrow pane navigation.
    /// winit 0.30 publishes modifier changes via `WindowEvent::ModifiersChanged`
    /// separately from key presses, so we keep a running copy and consult
    /// it inside the KeyboardInput arm.
    modifiers: winit::keyboard::ModifiersState,
}

impl App {
    pub fn new(
        evt_rx: std::sync::mpsc::Receiver<crate::transport::IncomingEvt>,
        rt: Option<tokio::runtime::Runtime>,
        cli: crate::cli::Cli,
        evt_tx: std::sync::mpsc::Sender<crate::transport::IncomingEvt>,
        req_tx: tokio::sync::mpsc::UnboundedSender<crate::transport::OutgoingReq>,
        req_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::transport::OutgoingReq>>,
    ) -> Self {
        Self {
            state: None,
            evt_rx: Some(evt_rx),
            rt,
            cli,
            evt_tx: Some(evt_tx),
            req_tx,
            req_rx,
            modifiers: winit::keyboard::ModifiersState::empty(),
        }
    }
}

/// Upload chunk size. Must stay well under the protocol frame cap (1 MiB, see
/// codec.rs): each chunk is base64-encoded into the JSON `file.upload` envelope,
/// which inflates it ~4/3 — so the old 1 MiB chunk became a ~1.4 MiB frame and
/// blew the cap, resetting the transport mid-upload and stranding the in-flight
/// state ("upload · already in progress" forever). 512 KiB → ~700 KiB base64 +
/// envelope, comfortably under the cap. The backend writes each chunk at its
/// offset, so chunk size is a frontend-only choice.
const UPLOAD_CHUNK: usize = 512 * 1024;

/// In-flight upload bookkeeping. The local file stays open; the chrome reads
/// the next `UPLOAD_CHUNK` from it on each `FileUploadAck`. `sent` is the byte
/// offset of the next chunk (also the cumulative bytes acked so far).
struct UploadState {
    file: std::fs::File,
    /// Absolute backend destination directory (the cursored nav folder).
    dir: String,
    /// `files:`-prefixed tree node id of the destination directory, so the
    /// nav listing can be refreshed when the upload completes (the new file
    /// shows up without a manual re-expand).
    dir_node_id: String,
    /// Basename sent to the backend (it sanitizes + de-dups).
    name: String,
    total: u64,
    sent: u64,
}

/// The visible region of an image preview in source-image pixel coords
/// (ADR 0022). Recomputed each draw; drives `capture_roi` + the `fe-state.json`
/// `preview` block. `path` is the source image's absolute path on the backend
/// (for LLM awareness); the crop itself is produced server-side by `image.crop`.
#[derive(Clone, Debug)]
struct PreviewRoi {
    node_id: String,
    path: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    src_w: u32,
    src_h: u32,
    zoom: f32,
}

/// Per-workspace auto-start info from `workspace.list` (contract b).
/// Spawned agent workspaces carry `autostart_claude = true` plus the
/// `agent_name`; the FE launches ccb on first attach. Interactive /
/// default workspaces have `autostart_claude = false`.
///
/// `task` (the old spawn brief) is now always empty: the FE no longer
/// delivers briefs (see the `workspace.list` handler — comm-spawn owns
/// task delivery via comm). Kept as a field so `autostart_claude_in_pane`'s
/// launch-only / deliver branch still compiles; it just never takes the
/// deliver branch.
#[derive(Clone)]
struct WsAutostart {
    autostart_claude: bool,
    #[allow(dead_code)] // informational; the bootstrap self-contains the join
    agent_name: String,
    task: String,
}

/// Phase of the post-launch task delivery to a spawned agent, driven from the
/// event loop (`advance_delivery`) so each step waits for the pane to settle
/// and re-checks the pinned target — never a blind timer.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DeliveryPhase {
    Boot,
    Interstitial,
    Typed,
    Submitted,
}

/// In-flight auto-start task delivery to a spawned agent's pane (contract b).
/// Replaces the old detached-thread blind-timer: `advance_delivery` watches the
/// pinned pane's pty for output to settle (readiness), keeps every keystroke
/// pinned to the launch session, double-Enter-submits, and surfaces a timeout
/// instead of silently leaving the agent idle.
struct AutoStartDelivery {
    pinned: String,
    task: String,
    agent: String,
    phase: DeliveryPhase,
    last_pty: std::time::Instant,
    started: std::time::Instant,
    submitted_at: Option<std::time::Instant>,
}

/// Pre-launch "sniff" gate (contract b). Set when a flagged agent pane
/// re-targets; `advance_autostart_scan` waits for tmux's replayed screen to
/// settle, then scans it for a *already-running* claude TUI. If found, the
/// launch is skipped (typing `claude …` into a live agent would land as a
/// prompt in its input box — the spam this guards against, esp. across FE
/// relaunches where the in-memory `autostarted_sessions` set is forgotten but
/// the agent's claude is still up). If not found, the real launch fires.
struct AutostartScan {
    session: String,
    started: std::time::Instant,
    last_pty: std::time::Instant,
}

/// How long an auto-start launch's "launching" hold suppresses a re-launch
/// (see `launching_sessions`). Covers ccb boot + the `advance_autostart_scan`
/// confirm latency (SETTLE 1.2s + ccb footer render, a few seconds), with
/// headroom. A launch lost to a pty re-target ages past this and retries on
/// the next attach; a real launch is promoted to `autostarted_sessions` by
/// the confirm-scan well before it expires.
const AUTOSTART_LAUNCH_HOLD: std::time::Duration = std::time::Duration::from_secs(12);

/// Heuristic: does this pane's visible text show a *running* Claude Code TUI?
/// Matches stable chrome present across boot/idle/working states so we don't
/// re-launch claude into a pane that already has it (which would type the
/// launch string into the live agent's prompt). Biased toward detection —
/// a false positive only means we skip autostart (user starts it manually,
/// surfaced via status), while a false negative re-introduces the spam.
fn pane_shows_running_claude(contents: &str) -> bool {
    let c = contents.to_lowercase();
    const MARKERS: [&str; 7] = [
        "for shortcuts",      // idle footer: "? for shortcuts"
        "esc to interrupt",   // working footer
        "bypass permissions", // --dangerously-skip-permissions banner
        "bypassing permissions",
        "welcome to claude code",
        "/help for", // help hint
        "for agents", // steady-state footer hint ("← for agents"),
                     // flag-independent — catches a claude started
                     // WITHOUT --dangerously-skip-permissions, whose
                     // footer carries no "bypass permissions" banner.
    ];
    MARKERS.iter().any(|m| c.contains(m))
}

/// True when a pane's tmux *foreground command* means claude is already
/// running there — `claude` (the name `pane_current_command` reports for the
/// CLI) or `node` (the runtime it may exec under). This is the *authoritative*
/// "don't autostart" signal the backend supplies on `pty.open`
/// (`PtyOpenRes.pane_command`), and unlike the screen-scrape
/// `pane_shows_running_claude` it does not depend on fresh post-attach output
/// or the FE-memory `autostarted_sessions` set — both of which mis-fired on an
/// idle, long-lived agent after an FE relaunch and spammed its live prompt
/// with the ccb launcher. Bias is toward *suppressing* a
/// launch: a false skip just means the user starts claude themselves, whereas
/// a false launch corrupts a working session's input.
fn pane_command_is_claude(cmd: Option<&str>) -> bool {
    match cmd {
        Some(c) => {
            let c = c.trim();
            c.eq_ignore_ascii_case("claude") || c.eq_ignore_ascii_case("node")
        }
        None => false,
    }
}

struct State {
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
    /// Miniature dark logo (from `LOGO_DARK_PNG`) flanking each session badge
    /// in the bottom strip, plus its native `(w, h)` for aspect-ratio sizing.
    /// `None` when the embedded PNG fails to decode — purely cosmetic, the
    /// strip renders unchanged without it.
    logo_quad: Option<(Quad, u32, u32)>,
    /// Small wordmark logo (from `LOGO_WORDMARK_PNG`) drawn at the top of the
    /// nav pane, plus its native `(w, h)` for aspect-ratio sizing. `None` when
    /// the embedded PNG fails to decode.
    wordmark_quad: Option<(Quad, u32, u32)>,
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
    /// the same zoom + pan. `None` until a PNG has loaded.
    preview_png_dims: Option<(u32, u32)>,
    /// The visible region of the current image preview, in *source-image
    /// pixel* coords (ADR 0022). Recomputed each draw from zoom/pan/letterbox
    /// when an image is shown; `None` when the preview isn't a croppable
    /// image. Consumed by `capture_roi` (the `C` hotkey / `capture_roi`
    /// fe-command) and surfaced in `fe-state.json` so the LLM pane knows what
    /// the user is zoomed into.
    preview_roi: Option<PreviewRoi>,
    /// Per-directory + per-dimensions cache of `(zoom, pan_px)` so
    /// switching among same-sized PNG renders in one directory restores
    /// the previous view rather than snapping back to fit. Mixed-size
    /// images miss the cache and start at default fit-to-pane.
    preview_png_cache: HashMap<(String, (u32, u32)), (f32, (f32, f32))>,
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
    /// Ctrl+M server-monitor drawer (ADR 0020): per-host metric ring + SVG
    /// chart renderer. `monitor_quad` is the rasterised chart painted into
    /// the drawer rect via the resvg→wgpu-quad path (same as MathJax);
    /// `monitor_rect_px` is the drawer's pixel rect from the last layout
    /// pass; `monitor_dirty` requests a re-render when data or size changes.
    monitor_view: crate::monitor_view::MonitorView,
    monitor_quad: Option<Quad>,
    monitor_rect_px: ScreenRect,
    monitor_dirty: bool,
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
    /// Drained at the top of every redraw; transport task pushes here.
    evt_rx: std::sync::mpsc::Receiver<crate::transport::IncomingEvt>,
    /// One-line status string for the chrome.
    status: String,
    /// Active NavTree modal prompt (Ctrl+N new-file, future delete-confirm),
    /// or `None` in the normal nav state. While `Some`, the NavTree key
    /// handler routes keystrokes into the prompt before any nav shortcut and
    /// the chrome shows the prompt on the status line.
    nav_prompt: Option<NavPrompt>,
    /// Node id of a file just created via the Ctrl+N prompt, awaiting its
    /// `file.write` reply. When that reply lands matching this id, we refresh
    /// the parent dir's listing so the new file appears, then clear this.
    /// `None` outside a create round-trip.
    pending_created_node_id: Option<String>,
    /// Node id of a file just deleted via the Ctrl+D confirm prompt, awaiting
    /// its `file.delete` reply. When that reply lands matching this id, we
    /// refresh the parent dir's listing so the row vanishes, then clear this.
    /// `None` outside a delete round-trip.
    pending_deleted_node_id: Option<String>,
    /// While `Some(t)` and `now < t`, the status line is holding a pushed
    /// notify (op::FE_COMMAND `notify`) and `rebuild_connection_status` won't
    /// clobber it — so a toast survives a workspace switch for a few seconds
    /// instead of being overwritten instantly (which otherwise made a pushed
    /// notify un-seeable while the user roamed workspaces). Cleared + the
    /// status rebuilt once elapsed (in `about_to_wait`, on the idle tick).
    notify_sticky_until: Option<std::time::Instant>,
    /// Files-mode tree, flattened for chrome rendering. Updated by
    /// `tree.root` / `tree.children` events; navigated by arrow keys.
    tree: TreeView,
    /// Which root tree the left pane is currently showing. `f`/`m` keys
    /// switch this and fire the corresponding wire request.
    mode: Mode,
    /// Annotation state for the most-recently-fired `concept.read`. `target`
    /// is what we asked for; when the response arrives with the same target,
    /// `exists` and `content` are filled. Mismatch (cursor moved before the
    /// reply landed) is dropped — the chrome stays on the last good answer
    /// until the new one arrives, which keeps the status line from flickering.
    concept_target_fired: Option<String>,
    concept: Option<ConceptInfo>,
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
    /// Sessions-mode (ADR 0013) per-pane capture dedup. Stores the
    /// `<session>/<pane_id>` we last fired `tmux.capture_pane` for; cursor
    /// moves clear it when we want a re-fetch. Cleared on mode-switch into
    /// Sessions so the first cursored pane fires fresh.
    tmux_capture_fired_for: Option<String>,
    /// Tmux session the BL pane is attached to. `None` until the first
    /// `pty.open` reply lands; defaults to `sot-llm` semantically.
    /// Sessions-mode Enter (B3) updates this to the targeted backend
    /// session and fires a fresh `pty.open` with the new target so the
    /// backend kills + respawns the pty.
    bl_pane_target: Option<String>,
    /// ADR 0014 active workspace. `None` resolves to the daemon's
    /// default workspace (the project the backend was launched with).
    /// `Some(slug)` routes tree/preview ops through the corresponding
    /// workspace's FilesMode + Kernel. Set when the user Enters a row
    /// in Sessions mode and persisted to `state-<hostname>.toml` so a
    /// restart resumes in the same workspace.
    active_workspace_id: Option<String>,
    /// ADR 0015 host registry loaded from `hosts.toml` at startup so
    /// `Mode::Hosts` can render a picker. Empty when the user hasn't
    /// configured any hosts (the picker shows a "no hosts configured"
    /// hint and the launcher uses its env-var defaults).
    hosts_config: crate::hosts::HostsConfig,
    /// Currently-targeted host slug, sourced from `state-<hostname>.toml`'s
    /// `last_host` at launch. Mirrors the launcher's view of which
    /// host the running tunnel points at. Updated when the user picks
    /// a row in `Mode::Hosts`; the launcher reads this on next launch.
    /// `None` means "use `hosts.toml::default_host`".
    selected_host: Option<String>,
    /// Two-press confirm for `D` (workspace destroy) in Sessions mode.
    /// First press arms with the cursor row's workspace_id; second
    /// press on the same row fires `workspace.destroy`. Cleared by
    /// any other keypress (snapshot-then-reset at the top of the
    /// input handler) so a cursor move disarms the trap.
    pending_destroy_target: Option<String>,
    /// Short hostname from the hello response, kept alongside the
    /// daemon's project_root basename so the chrome can rebuild the
    /// "connected · host:workspace · rev N" status line every time the
    /// active workspace changes — not just at connect time.
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
    /// Absolute path of the most recent `project.scan`'s `project_root`.
    /// The chrome strips this prefix off the absolute file paths each
    /// Modules-mode entry carries to synthesize the `files:<relpath>`
    /// node id that `preview.get` expects. Reset on each scan reply.
    scan_project_root: Option<String>,
    /// Highest revision seen in any frame, surfaced in the status line.
    /// Tracked separately so post-connect events (workspace switches,
    /// preview.changed bumps) don't render a stale rev.
    last_revision: u64,
    /// Slug → label map populated from each `workspace.list` reply. Used
    /// by `rebuild_connection_status` to show a friendly workspace name
    /// in the chrome status (e.g. "Alpha") rather than the raw slug.
    workspace_labels: HashMap<String, String>,
    /// Slug → project_root map populated from each `workspace.list`
    /// reply. Used to resolve `files:<rel>` node ids to absolute paths
    /// in the *active* workspace (not the daemon's startup workspace).
    /// Falls back to `daemon_project_root` if the active workspace
    /// hasn't appeared in a `workspace.list` reply yet.
    workspace_project_roots: HashMap<String, String>,
    /// Slug → (agent_state, agent_status_at) from each `workspace.list`
    /// reply, so the bottom session strip can colour each name by its
    /// agent's work-state (the same data the Sessions-mode rows carry in
    /// their node payload). Refreshes live on the daemon's registry-watch
    /// `workspace.changed` push; empty entries render with the default
    /// strip styling.
    workspace_states: HashMap<String, (String, String)>,
    /// Badge floor (ADR 0025 §1): slug → workspace-relative path of a
    /// `nav.preview` result that arrived for a workspace the FE wasn't
    /// viewing. Instead of silently dropping the off-workspace result (the
    /// bug §1 fixes — a backend session pushes a result to a FE looking at
    /// another workspace and it vanishes), we record it here ("result
    /// pending") and badge that workspace's row/strip name non-disruptively.
    /// Latest-wins per workspace. When the user later *switches* to the
    /// workspace, `switch_to_workspace` drives the pending preview and clears
    /// the entry, so a result always reaches the user — never dropped. The
    /// future `op::FE_COMMAND` handler will reuse `mark_pending_nav`.
    pending_nav: HashMap<String, String>,
    /// Slug → the *previous* work-state string we last saw, so the
    /// `workspace_states` update site can tell a real transition (a slug that
    /// had a known, different prior state) from a first-ever appearance. Only
    /// real transitions flash; a slug showing up for the first time does not.
    prev_workspace_states: HashMap<String, String>,
    /// Slug → the `Instant` its work-state last changed, driving the
    /// status-change *flash* (the name brightens toward white then fades over
    /// `FLASH_SECS`). Entries are pruned once they age past the fade so the
    /// map stays small, and while any entry is live `about_to_wait` schedules
    /// a faster repaint so the fade animates.
    flash_starts: HashMap<String, std::time::Instant>,
    /// Selected-session contrast lever (`--contrast-mode`, ADR 0023). `false`
    /// = "bright" (default): the selected/active row pops by going brighter +
    /// bold. `true` = "dim": non-selected rows are dimmed so the selection
    /// pops by contrast. Applied in both the nav rows and the bottom strip.
    contrast_dim: bool,
    /// Ordered list of workspace slugs from the most recent
    /// `workspace.list` reply (backend sorts alphabetically by slug).
    /// Drives the Shift+ArrowLeft / Shift+ArrowRight cycle hotkey (D7) —
    /// the "next" workspace is the next slug in this vec, wrapping at both
    /// ends. Empty until the first reply lands.
    workspace_slugs: Vec<String>,
    /// Slug of the workspace flagged `is_default` in `workspace.list`.
    /// Used to interpret `active_workspace_id == None` as "we're on the
    /// default workspace" when locating the current position in
    /// `workspace_slugs` during a cycle. `None` until the reply lands.
    default_workspace_slug: Option<String>,
    /// tmux_session → auto-start info, from each `workspace.list` reply
    /// (contract b). `attach_session_to_bl` looks the just-attached
    /// session up here to decide whether to launch claude + deliver the
    /// bootstrap. Keyed by tmux_session (what attach receives).
    workspace_autostart: HashMap<String, WsAutostart>,
    /// tmux sessions whose claude auto-start is CONFIRMED up — recorded only
    /// by the `advance_autostart_scan` sniff (4171) when it actually sees ccb
    /// running, so a re-attach doesn't spawn a second claude. In-memory only:
    /// a FE relaunch forgets this — but the scan re-confirms a still-running
    /// agent on the next attach, so the forgotten set self-heals.
    autostarted_sessions: std::collections::HashSet<String>,
    /// tmux session → the `Instant` we last *fired* an auto-start launch into
    /// it. A time-bounded "launching" hold (NOT a permanent mark): while a
    /// stamp is within `AUTOSTART_LAUNCH_HOLD` the attach guard skips
    /// re-launching (so a rapid re-attach during ccb boot can't double-fire
    /// `ccb` into the first instance's prompt). A launch lost to a pty
    /// re-target is never confirmed by the scan, so its stamp simply ages
    /// out — the guard then stops skipping and the session retries on the
    /// next attach. The 4171 confirm-scan promotes a stamp into
    /// `autostarted_sessions` (and clears it here). Fixes the old eager-mark
    /// race where a lost launch left the session permanently marked → a bare
    /// shell that never retried.
    launching_sessions: HashMap<String, std::time::Instant>,
    /// Set by `attach_session_to_bl` when the just-attached session is a
    /// flagged agent workspace not yet started; consumed by the matching
    /// `PtyOpened` so the launch lands *after* the pty re-target lands.
    pending_autostart: Option<String>,
    /// Pre-launch sniff gate: between the pty re-target and the actual
    /// claude launch, `advance_autostart_scan` waits for the replayed screen
    /// to settle and skips the launch if claude is already running there.
    autostart_scan: Option<AutostartScan>,
    /// In-flight auto-start task delivery (contract b), or `None`. Driven by
    /// `advance_delivery` from the event loop each tick: it waits for the
    /// pinned pane's output to settle (readiness), types the bootstrap,
    /// double-Enter-submits, re-checks `bl_pane_target == pinned` before every
    /// keystroke (never misroutes), and on timeout surfaces a status notice
    /// rather than leaving a launched-but-idle agent.
    delivery: Option<AutoStartDelivery>,
    /// A delivery parked by a BL switch-away mid-flight (the symmetric
    /// counterpart of the launch defer in `advance_autostart_scan`). Resumed
    /// phase-preserved by the `PtyOpened` for the next attach of its pinned
    /// session: a delivery deferred at `Typed` already has the task text
    /// sitting in the agent's input box (tmux kept the pane alive), so
    /// resuming there just submits — re-typing would double the text.
    deferred_delivery: Option<AutoStartDelivery>,
    /// Sessions-mode workspace picker (ADR 0014). `Some(state)` while
    /// the user is browsing a directory tree to pick the project_root
    /// of a new workspace; `None` outside the picker. Supersedes the
    /// older label-only prompt that fired `tmux.create_session` with
    /// a hardcoded `$SOT_PROJECTS_ROOT/<label>` cwd.
    workspace_picker: Option<WorkspacePicker>,
    /// Per-workspace NavTree snapshots. Captured when the user
    /// switches away from a workspace; restored when they switch back.
    /// Keyed by workspace slug ("<default>" for the daemon-default).
    /// Means switching workspaces doesn't lose cursor position or the
    /// expanded-folder shape of the file tree.
    workspace_ui_snapshots: HashMap<String, WorkspaceUiSnapshot>,
    /// Per-workspace REPL snapshots. Captured separately from the UI
    /// snapshot because `ReplEvalDone` replies can land for workspaces
    /// the user has swapped away from, and those need to mutate the
    /// owning workspace's log directly. Keyed the same way (`<default>`
    /// for the daemon-default workspace).
    workspace_repl_snapshots: HashMap<String, WorkspaceReplSnapshot>,
    /// Tracks which workspace each in-flight eval belongs to. Set at
    /// `submit_repl_input` time using the live `active_workspace_id`;
    /// consumed by `ReplEvalDone` so the reply can route to the right
    /// workspace's log even if the user has swapped away.
    eval_id_workspace: HashMap<u64, String>,
    /// Per-eval display info for an in-flight streaming `repl.run_file`,
    /// stashed when the *acceptance* ack lands (it carries the resolved
    /// basename/project/fresh but a 0 elapsed — the run hasn't happened yet)
    /// and consumed when the streamed `Done` frame finalizes, so the
    /// completion status line shows the real elapsed. Keyed by eval_id:
    /// `(basename, project_dir, fresh)`.
    repl_runfile_status: HashMap<u64, (String, Option<String>, bool)>,
    /// Shaped annotation body for the latest `concept.read` reply, ready
    /// for the chrome's concept pane to render. `None` when the cursored
    /// row has no annotation; rebuilt on every event so we don't pay the
    /// markdown shape cost per frame.
    preview_concept: Option<MarkdownPreview>,
    /// Pixel rect of the concept pane from the most recent layout pass.
    /// Cached so `resize` triggers only on actual shape changes.
    concept_rect_px: ScreenRect,
    /// `kernel.request file.parse` results, keyed by the relative path the
    /// kernel was asked about (matches the suffix of `files:` node ids).
    /// Used by the drift badge: if a row's path has a hash here AND its
    /// annotation parses a `synced_against`, and the two differ, yellow it.
    /// Grows as the user navigates — phase-2 may sweep eagerly.
    file_ast_hashes: std::collections::HashMap<String, String>,
    /// Paths the GPU thread has already asked `file.parse` for. Prevents
    /// re-firing while a request is in flight. Cleared on disconnect.
    file_parse_fired: std::collections::HashSet<String>,
    /// One-shot from `--start-selected <n>`; consumed by the first tree.root
    /// or modules.list response that lands so the cursor opens on that row.
    /// `None` after consumption (or if the flag wasn't set).
    pending_initial_selection: Option<usize>,
    /// One-shot nav cursor restore across an ADR-0017 relaunch: the
    /// persisted `(selected node id, scroll)`. Applied best-effort when
    /// the matching workspace's `tree.root` arrives and the row is
    /// present; a deeply-collapsed selection that isn't in the freshly
    /// loaded tree just lands on the default cursor.
    pending_resume_nav: Option<(String, u16)>,
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
    /// One-shot PER WORKSPACE: on a workspace's first Files-mode `tree.root`
    /// where nothing else (ADR-0017 resume, `--start-selected`) placed the
    /// cursor, default it to the project README so a fresh session opens
    /// onto rendered docs instead of the bare root row. Keys are
    /// `current_workspace_key()`; presence = already defaulted (or resumed)
    /// once, so refreshes never yank the cursor.
    nav_readme_defaulted: std::collections::HashSet<String>,
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
    /// Push-side of the GPU→transport channel. `None` in offline mode (no
    /// transport spawned), in which case Right/Enter on an expandable node
    /// just no-ops with a chrome hint.
    req_tx: tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
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
    /// Harness instance (`--ephemeral`, or any `--capture` run): never the
    /// user's primary FE on this host, so it must not touch the per-host
    /// shared state — no resume-state / `fe-state.json` writes, no state
    /// restore, and no consumption of `fe-commands/` or the relaunch
    /// sentinel (both watchers DELETE what they read — a harness eating
    /// the primary's relaunch signal or control command is the B8 bug
    /// class). Single-writer rule for multi-FE hosts.
    ephemeral: bool,
    frame_counter: u32,
    /// True after a successful capture; the WindowEvent handler reads this
    /// next event-loop iteration and calls `event_loop.exit()`.
    should_exit: bool,
    /// Label of the most recent key press (for chrome feedback). `None` until
    /// the user hits a key; modes-mode + tree navigation hang off the same
    /// keyboard input plumbing once they land.
    last_key: Option<String>,
    /// Cached battery readout for the top-right chrome (e.g. `85%` or `+72%`
    /// while charging). `None` means no battery present / query failed — we
    /// render nothing in that case (never a fake `0%`). The OS query is not
    /// free, so it's refreshed at most once per `BATTERY_QUERY_INTERVAL`; the
    /// clock ticking every second reuses this cached value in between.
    battery_label: Option<String>,
    /// When the cached `battery_label` was last (re)computed. `None` forces a
    /// query on the first paint.
    last_battery_query: Option<std::time::Instant>,
    /// Frame-rate cap state. `request_redraw` from event handlers and the
    /// transport task queue `RedrawRequested`; if we'd draw twice within
    /// `FRAME_BUDGET`, the second one sets `dirty` and `about_to_wait`
    /// reschedules at the next frame boundary so a burst (paste, PTY echo
    /// storm, LLM token stream) collapses into one frame.
    dirty: bool,
    last_frame_at: Option<std::time::Instant>,
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
    /// bottom-strip logo bookends (radians), the live angular velocity a
    /// workspace cycle flicks it with, and the last spin-frame timestamp for
    /// frame-rate-independent decay (`None` when at rest).
    wheel_angle: f32,
    wheel_vel: f32,
    wheel_anim_last: Option<std::time::Instant>,
    /// Which pane has keyboard focus. Tree by default; Ctrl+Arrow moves
    /// focus. REPL focus consumes character keys as code rather than
    /// firing tree navigation.
    focus: PaneFocus,
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
    /// Persistent scroll offset for the nav pane (vim-style scrolloff
    /// behaviour). Updated each frame from the cursor's position relative
    /// to the current viewport: when the cursor moves into the bottom
    /// 1/3 of the pane going down, scroll keeps it stationary there;
    /// same on the way up. At the body's edges the cursor falls through
    /// to the actual top/bottom row. Kept on State so the cursor
    /// position alone doesn't determine the scroll — direction of motion
    /// matters.
    tree_scroll: u16,
    /// vt100 terminal emulator backing the LLM pane. Bytes streamed
    /// from the backend's pty (`pty.evt`) get fed in via `process`,
    /// and `screen()` is walked into the BL content rect on every
    /// redraw. Sized to the BL content rect; resized when the rect
    /// changes shape. Named `pty_terminal` so it doesn't collide
    /// with the ratatui `terminal: Terminal<WgpuBackend>` field.
    pty_terminal: vt100::Parser,
    /// Last (cols, rows) we sent the backend so it can size its pty.
    /// `None` = no pty.open sent yet; first BL redraw with a real rect
    /// triggers the open. Subsequent redraws compare the BL content
    /// rect to this and fire `pty.resize` on mismatch.
    pty_size: Option<(u16, u16)>,
    /// Scroll offset (rows from the tail) of the REPL pane. 0 = live,
    /// positive = looking at older lines. Reset to 0 whenever the user
    /// types into the REPL so typing always snaps to live; otherwise
    /// updated by the mouse wheel when the cursor is over the BR pane.
    /// Clamped to [0, total_lines - viewport_h] at render time.
    repl_scroll: u16,
    /// Scroll offset (rows from the top) of the preview pane's flowed
    /// text. 0 = top of the markdown body; positive = scrolled down.
    /// Drives an upward pixel shift of the cosmic-text TextArea via
    /// `ExtraArea::scroll_y_px`. Image-only previews (PNG/SVG) ignore
    /// this. Clamped at render time to total_layout_lines minus visible
    /// rows so the user can't scroll past the bottom of the content.
    preview_scroll: u16,
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
    /// Scroll offset of the LLM pty: rows *above* the live screen.
    /// 0 = live (the bottom of the scrollback ring); positive =
    /// looking back at older bytes. Implemented via vt100-ctt's
    /// `Screen::set_scrollback`, which the redraw applies just before
    /// painting. Reset to 0 whenever the user types into the pty
    /// (snap-to-live on keystroke) and on rect resize (the row map
    /// changes shape, so the old offset is meaningless).
    pty_scroll: u16,
    /// The four pane content rects from the most recent redraw, cached
    /// so keyboard handlers can size scroll steps to a real viewport
    /// (`PgUp/PgDn`, `Ctrl+u/d`). Updated at the end of every redraw.
    pane_rects: PaneRects,
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
    /// Time of the last SGR mouse-wheel sequence forwarded to the LLM
    /// pty. Used to throttle wheel passthrough: each SGR triggers a
    /// tmux pane repaint round-tripped through SSH, so high-rate
    /// touchpad events would queue up and scroll would lag behind the
    /// wheel. Capping fires keeps it responsive at the cost of
    /// "skipping" excess events when scrolling fast.
    last_pty_wheel_at: Option<std::time::Instant>,
    /// When `true`, the currently-focused pane fills the whole window;
    /// the other three are zero-sized and not drawn. Toggled with
    /// `Action::MaximizePane` / `Action::RestoreLayout` (defaults
    /// `Alt+=` / `Alt+-`). Maximisation tracks `focus` rather than
    /// being pinned to a specific pane — Ctrl+Arrow while
    /// maximised swaps which pane is on screen, which is what the user
    /// wants when they're staring at one pane and want to peek at
    /// another.
    maximized: bool,
    /// Resolved keybindings (defaults overlaid with the user's
    /// `keybindings.toml` if present). See `keybindings.rs` for the
    /// file format and discovery order. Read-only once loaded — the
    /// chrome doesn't reload mid-session.
    bindings: KeyBindings,
    /// User-tunable chrome settings (layout proportions today, future
    /// general settings). Loaded once at startup via the same layered
    /// discovery as `bindings`; not re-read on file change.
    settings: Settings,
    /// In-flight `file.upload`, if any. The chrome drives chunk flow control:
    /// it sends chunk 0 in `start_upload`, then sends the next chunk on each
    /// non-`done` `FileUploadAck`. `None` when no upload is running.
    upload: Option<UploadState>,
    /// Primary monitor aspect ratio captured at startup (width /
    /// height). Used to resolve `settings.preset = "auto"` to a named
    /// preset. Locked for the session — resizing the window doesn't
    /// re-pick a different preset; the user explicitly avoided
    /// in-session reflow.
    monitor_aspect: f32,
    /// Bottom drawer state — `Closed`, `Repl` (Ctrl+J), or `Terminal`
    /// (Ctrl+T). When open it takes its configured fraction of vertical
    /// space and the columns shrink. The variant selects which content
    /// renders; `layout::compute` only needs `drawer.is_open()`.
    drawer: DrawerContent,
    /// Local PTY terminal hosting the OS shell (G2). Lazily spawned the
    /// first time the Terminal drawer opens (Ctrl+T); a separate field
    /// from the ratatui `terminal` so the draw closure's `self.terminal`
    /// borrow and this terminal's `screen()` borrow are disjoint.
    local_term: Option<crate::term::LocalTerminal>,
    /// Last `(cols, rows)` the local terminal's PTY was sized to. `None`
    /// until the drawer rect is first observed; drives resize-on-change
    /// (mirrors `pty_size` for the LLM pane).
    term_size: Option<(u16, u16)>,
    /// Scrollback offset (rows up from the bottom) for the local terminal
    /// drawer, applied via `Screen::set_scrollback` each draw — mirrors
    /// `pty_scroll` for the LLM pane. Only used when the running app hasn't
    /// grabbed the mouse; mouse-aware apps (vim/less) get wheel events
    /// forwarded as SGR sequences instead. Reset to 0 (live tail) on input.
    term_scroll: u16,
    /// Local repo root (`$SOT_REPO_DIR`, set by the supervisor). Used as
    /// the Terminal drawer's working directory so `claude --continue`
    /// resumes the right project's session. `None` when launched outside
    /// the supervisor (then the shell inherits the frontend's cwd). ADR 0017.
    repo_dir: Option<std::path::PathBuf>,
    /// One-shot command run in the Terminal drawer on its first spawn after
    /// a `--relaunched` start (the configured `[terminal] resume_command`,
    /// default [`crate::settings::DEFAULT_RESUME_COMMAND`]). `take()`n on
    /// first use so a later
    /// Ctrl+T reopen doesn't re-run it. ADR 0017.
    pending_resume_command: Option<String>,
    /// Set by the relaunch-watcher thread when the sentinel file
    /// (`%LOCALAPPDATA%\sot\relaunch.request`) appears. The window-event
    /// handler observes it and exits with code 75 so the supervisor restages
    /// the freshly-built binary and respawns us with `--relaunched`. ADR 0017.
    relaunch_flag: Arc<std::sync::atomic::AtomicBool>,
    /// FE control commands (ADR 0019) enqueued by the command-file watcher
    /// thread (the producer) and drained on the main thread in `window_event`
    /// (the consumer), so dispatch runs the same code paths as the keybinds.
    fe_commands: Arc<std::sync::Mutex<std::collections::VecDeque<FeCommand>>>,
    /// Hash of the last `fe-state.json` we wrote (ADR 0019), so the readback
    /// file is only rewritten when the observable state actually changes.
    fe_state_sig: Option<u64>,
    /// One-shot: focus + raise the window on the first rendered frame.
    /// A focus request made at window-creation time (before the window
    /// is shown / before the first paint) is widely ignored by window
    /// managers — Windows' foreground-lock and macOS both restrict it.
    /// Deferring to the first frame is the portable way to land focused
    /// on launch / after an ADR-0017 self-relaunch. When the OS blocks
    /// focus-stealing outright we fall back to `request_user_attention`
    /// (taskbar flash / dock bounce / urgent hint). Cleared after use.
    focus_on_first_frame: bool,
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
    /// `?` toggles a keybindings help overlay over the preview pane.
    /// `help_open` gates it; `preview_help` is the shaped (monospace)
    /// buffer, rebuilt on open + on resize. Independent of `edit_state`
    /// and takes draw precedence over both edit and the normal preview.
    help_open: bool,
    preview_help: Option<MarkdownPreview>,
    /// ADR 0030 §2: a persistent, blocking "update needed" message shown when
    /// the backend refused the handshake on a protocol-version skew. `Some`
    /// holds the pre-formatted body (versions + protocols + dev fix hint);
    /// while set it takes draw precedence over help, edit, and the normal
    /// preview (it renders where the help overlay does, reusing that path).
    /// Cleared on the next successful `Connected`. `preview_fatal` is the
    /// shaped buffer, rebuilt on set + on resize (mirrors `preview_help`).
    protocol_mismatch: Option<String>,
    preview_fatal: Option<MarkdownPreview>,
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
    /// F5 fires this to collapse the transport's exponential-backoff
    /// sleep and attempt an immediate reconnect — useful when wifi
    /// flickers and the user knows it's back before the current
    /// backoff cycle would have noticed. Held on State (not App) so
    /// the keyboard handler reaches it via &mut state.
    reconnect_now: Arc<tokio::sync::Notify>,
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
    /// Runtime multiplier on top of the startup `scale`. `1.0` = no
    /// change; `>1.0` = bigger fonts and cells; `<1.0` = smaller.
    /// Bumped via `Ctrl+=` / `Ctrl+-` (reset by `Ctrl+0`). Affects
    /// chrome cell metrics, the TextLayer's per-line metrics, and the
    /// preview's flowed-text buffer simultaneously (user picked
    /// "Global only" — same change applies to every pane).
    text_scale_mult: f32,
    /// Last preview source (mime + raw bytes) cached so a font-size
    /// change can rebuild the preview at the new scale without a
    /// round-trip back to the backend. Cleared on disconnect.
    preview_src: Option<(String, Vec<u8>)>,
}

/// Wire shape for `application/vnd.sot.tokens+json` from the
/// kernel-side JuliaSource plugin (`53a46d8`). Concatenating
/// every span's `text` is guaranteed to reproduce the source file
/// byte-for-byte — kinds are advisory for colouring.
#[derive(Debug, serde::Deserialize)]
struct TokensPayload {
    spans: Vec<TokenSpan>,
}

#[derive(Debug, serde::Deserialize)]
struct TokenSpan {
    text: String,
    kind: String,
}

/// Cached pane geometry. `tl` and `bl` are reserved for future use
/// (click-to-focus, per-pane mouse interactions); only `tr` / `br` are
/// read today for keyboard viewport sizing.
#[derive(Default, Clone, Copy, Debug)]
#[allow(dead_code)]
struct PaneRects {
    nav: ratatui::layout::Rect,
    preview: ratatui::layout::Rect,
    llm: ratatui::layout::Rect,
    /// Drawer (REPL) rect when the drawer is open; zero-area when
    /// closed. Consumers using `.repl.height` already handle zero
    /// safely (clamp to 1 / no-op).
    repl: ratatui::layout::Rect,
}

/// Letterbox an image of `(iw, ih)` into `outer`, preserving aspect ratio.
/// Cache key for the PNG preview view-state cache. Takes a `files:<rel>`
/// node id and a `(w, h)` and returns the parent-dir portion of the id
/// (everything up to and including the last `/`) paired with the dims.
/// `None` if the node id is missing or doesn't fit the expected shape,
/// in which case the caller falls back to fit-to-pane defaults.
fn png_cache_key_from_node_id(
    node_id: Option<&str>,
    dims: (u32, u32),
) -> Option<(String, (u32, u32))> {
    let id = node_id?;
    // The `files:` prefix is opaque to us; we just want the directory
    // portion so two PNGs in the same dir share a key. Anything else
    // (no `/`) means the file is at the project root — use the empty
    // prefix string, which is still a valid HashMap key.
    let parent = id.rfind('/').map(|i| &id[..=i]).unwrap_or("");
    Some((parent.to_string(), dims))
}

/// Translate a "desired visible sRGB colour" into the `wgpu::Color` that
/// `LoadOp::Clear` should carry, so the same painted bg appears the same
/// across machines.
///
/// Why: wgpu interprets clear values per the surface format. On an sRGB
/// target (`Bgra8UnormSrgb`) the value is treated as **linear-light** and
/// the hardware sRGB-encodes it on write — so a `0.02` clear comes out
/// at sRGB ≈ `#272727` (dark gray). On a non-sRGB target (`Bgra8Unorm`)
/// the value is the literal pixel — same `0.02` lands as `#050505`
/// (near-black). Without this conversion, two machines whose adapters
/// happen to land on different swapchain formats render the chrome at
/// different brightness levels.
fn clear_color_for_surface(visible_srgb: (f64, f64, f64), is_srgb_target: bool) -> wgpu::Color {
    let convert = |c: f64| {
        if !is_srgb_target {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    wgpu::Color {
        r: convert(visible_srgb.0),
        g: convert(visible_srgb.1),
        b: convert(visible_srgb.2),
        a: 1.0,
    }
}

fn letterbox(outer: ScreenRect, img_px: (u32, u32)) -> ScreenRect {
    let (iw, ih) = (img_px.0.max(1) as f32, img_px.1.max(1) as f32);
    let img_aspect = iw / ih;
    let outer_aspect = outer.w / outer.h.max(1.0);
    let (w, h) = if img_aspect > outer_aspect {
        (outer.w, outer.w / img_aspect)
    } else {
        (outer.h * img_aspect, outer.h)
    };
    ScreenRect {
        x: outer.x + (outer.w - w) * 0.5,
        y: outer.y + (outer.h - h) * 0.5,
        w,
        h,
    }
}

/// On-screen size ceiling for a single source pixel, in screen pixels per
/// axis. PNG-preview zoom is capped so one source pixel never grows past
/// 16×16 screen px — large enough to inspect individual cells of a dense
/// scientific raster, small enough that an already-magnified tiny image
/// can't blow up without bound. This replaced a fixed `32×`-fit cap: the
/// meaningful limit is pixel magnification, not a multiple of fit-to-pane
/// (user ask 2026-05-29). See `png_zoom_max`.
const MAX_PX_PER_SRC_PX: f32 = 16.0;

/// Upper zoom bound for a native-`img_px` PNG shown fitted into a
/// `pane_w × pane_h` (physical-pixel) pane. Zoom multiplies the letterbox
/// (fit-to-pane) scale, which preserves aspect — so at zoom 1 a source pixel
/// already spans `fit = min(pane_w/iw, pane_h/ih)` screen px, and the 16-px
/// ceiling is hit at `MAX_PX_PER_SRC_PX / fit`. Clamped to ≥ 1.0 so
/// fit-to-pane is always reachable, even for an image already magnified past
/// the ceiling at fit (tiny image in a large pane). Degenerate `fit`
/// (zero/non-finite) falls back to the bare ceiling.
/// Map the visible region of a zoomed/panned image preview to a
/// source-image-pixel ROI (ADR 0022). The full image is drawn into
/// `canvas` (top-left `canvas_x,canvas_y`, size `canvas_w×canvas_h`); the
/// visible window is `pane` (the scissor). The visible canvas∩pane rectangle,
/// as fractions of the canvas, maps directly to the same fractions of the
/// source — so this is independent of any decode-time downsample (the caller
/// passes native `src_w,src_h`). Returns `(x, y, w, h)` in source px, clamped
/// to the image, or `None` if nothing is visible / inputs are degenerate.
#[allow(clippy::too_many_arguments)]
fn visible_roi_px(
    canvas_x: f32,
    canvas_y: f32,
    canvas_w: f32,
    canvas_h: f32,
    pane_x: f32,
    pane_y: f32,
    pane_w: f32,
    pane_h: f32,
    src_w: u32,
    src_h: u32,
) -> Option<(u32, u32, u32, u32)> {
    if src_w == 0 || src_h == 0 || canvas_w <= 0.0 || canvas_h <= 0.0 {
        return None;
    }
    let vx0 = canvas_x.max(pane_x);
    let vy0 = canvas_y.max(pane_y);
    let vx1 = (canvas_x + canvas_w).min(pane_x + pane_w);
    let vy1 = (canvas_y + canvas_h).min(pane_y + pane_h);
    if vx1 <= vx0 || vy1 <= vy0 {
        return None;
    }
    let fx0 = ((vx0 - canvas_x) / canvas_w).clamp(0.0, 1.0);
    let fy0 = ((vy0 - canvas_y) / canvas_h).clamp(0.0, 1.0);
    let fx1 = ((vx1 - canvas_x) / canvas_w).clamp(0.0, 1.0);
    let fy1 = ((vy1 - canvas_y) / canvas_h).clamp(0.0, 1.0);
    let x = (fx0 * src_w as f32).floor() as u32;
    let y = (fy0 * src_h as f32).floor() as u32;
    let w = (((fx1 - fx0) * src_w as f32).ceil() as u32).clamp(1, src_w - x);
    let h = (((fy1 - fy0) * src_h as f32).ceil() as u32).clamp(1, src_h - y);
    Some((x, y, w, h))
}

fn png_zoom_max(pane_w: f32, pane_h: f32, img_px: (u32, u32)) -> f32 {
    let iw = img_px.0.max(1) as f32;
    let ih = img_px.1.max(1) as f32;
    let fit = (pane_w / iw).min(pane_h / ih);
    if fit.is_finite() && fit > 0.0 {
        (MAX_PX_PER_SRC_PX / fit).max(1.0)
    } else {
        MAX_PX_PER_SRC_PX
    }
}

/// Read the OS clipboard as text. Returns `None` (logging at warn) on
/// clipboard failure or empty contents so callers fall through without
/// panicking. winit does not deliver paste events on Windows (see
/// `Cargo.toml`), so every paste path goes through an explicit clipboard
/// read.
fn read_clipboard_text() -> Option<String> {
    let mut cb = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "clipboard.new failed — paste dropped");
            return None;
        }
    };
    match cb.get_text() {
        Ok(t) if !t.is_empty() => Some(t),
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "clipboard.get_text failed — paste dropped");
            None
        }
    }
}

/// Wrap clipboard text in the bracketed-paste envelope. Newlines are
/// normalized to `\r` (matching the Enter handlers that drive the ptys) so a
/// paste behaves exactly like the user retyping the text; the
/// `\e[200~ ... \e[201~` envelope tells the receiving CLI "this is one
/// paste, don't run line-by-line".
fn bracketed_paste_bytes(text: &str) -> Vec<u8> {
    let normalized: String = text.replace("\r\n", "\r").replace('\n', "\r");
    let mut bytes: Vec<u8> = Vec::with_capacity(normalized.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(normalized.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

/// Read the OS clipboard and forward it to the LLM pane's pty as one
/// bracketed-paste blob. Returns `false` (and logs at warn) on clipboard or
/// transport failure so the caller can fall through without panicking;
/// otherwise `true`.
fn forward_clipboard_paste_to_llm(state: &mut State) -> bool {
    let Some(text) = read_clipboard_text() else {
        return false;
    };
    let bytes = bracketed_paste_bytes(&text);
    state.pty_scroll = 0;
    if let Err(e) = state.req_tx.send(OutgoingReq::PtyWrite { bytes }) {
        tracing::warn!(error = %e, "drop pty.write (paste) — channel closed");
        return false;
    }
    true
}

/// Read the OS clipboard and forward it to the local Terminal drawer's pty as
/// one bracketed-paste blob. Mirrors `forward_clipboard_paste_to_llm` but
/// targets the in-process `local_term` rather than the remote pty.
fn forward_clipboard_paste_to_local_term(state: &mut State) {
    let Some(text) = read_clipboard_text() else {
        return;
    };
    let bytes = bracketed_paste_bytes(&text);
    if let Some(t) = state.local_term.as_mut() {
        t.send_input(&bytes);
        state.term_scroll = 0;
    }
}

fn cell_grid_for(
    width: u32,
    height: u32,
    cell_w: f32,
    cell_h: f32,
    ox: f32,
    oy: f32,
) -> (u16, u16) {
    let cols = ((width as f32 - 2.0 * ox).max(0.0) / cell_w).floor() as u16;
    let rows = ((height as f32 - 2.0 * oy).max(0.0) / cell_h).floor() as u16;
    (cols.max(1), rows.max(1))
}

/// Minimum time between frames in interactive mode (~120 fps). Picks the
/// tightest cap that still gives a paste burst, a PTY echo storm, and the
/// keystroke that fired them room to coalesce into one frame, since the
/// monitor can't display faster than its refresh rate anyway.
const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_micros(8_333);

/// Settle delay for cursor-driven backend round-trips (`preview.get`,
/// `concept.read`, `file.parse` drift check, `tmux.capture_pane`). Without
/// this gate, hold-to-scroll generates one round-trip per visited row —
/// hundreds per second for fast scroll — which saturates the SSH tunnel
/// and renders many heavy preview blobs through wgpu in rapid succession.
/// Symptom: transport reconnect (which then re-fires `tree.root` and
/// resets the cursor to row 0) + GPU pressure (AMD driver overlay fires).
/// 150ms is short enough to feel instant on settle, long enough to absorb
/// any realistic auto-repeat rate.
const NAV_FIRE_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(150);

impl State {
    fn new(
        event_loop: &ActiveEventLoop,
        evt_rx: std::sync::mpsc::Receiver<crate::transport::IncomingEvt>,
        cli: &crate::cli::Cli,
        req_tx: tokio::sync::mpsc::UnboundedSender<OutgoingReq>,
    ) -> Result<Self> {
        // Restore previous window geometry on launch. Saved in logical
        // pixels so cross-DPR launches behave sensibly. Defaults are
        // ~50% bigger than the spike's original 1024×700.
        let persisted_geom = crate::state_persistence::load();
        let init_w = persisted_geom.window_w.unwrap_or(1536.0);
        let init_h = persisted_geom.window_h.unwrap_or(1050.0);
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
            const LOGO_PNG: &[u8] = include_bytes!("../../../logo.png");
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

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .context("no compatible wgpu adapter found")?;

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
            present_mode: surface_caps.present_modes[0],
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
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
        // uses it as its cwd so `claude --continue` resumes the right
        // project's session.
        //
        // Open the drawer + arm the one-shot resume command on startup when
        // EITHER we're resuming a self-relaunch (`--relaunched`) OR a
        // `[terminal] resume_command` is configured — so a plain user
        // start/restart also bootstraps the dogfood session (e.g. re-arming
        // fast comm via `/sot-fe-session-start`), not just the exit-75 loop.
        // With neither, the FE opens clean (drawer closed).
        let settings = Settings::load_layered();
        // The ADR-0017 supervisor exports SOT_REPO_DIR so the resume shell lands
        // in the repo dir rather than $HOME (the wrong-folder relaunch, observed
        // 2026-06-25).
        let repo_dir = std::env::var_os("SOT_REPO_DIR").map(std::path::PathBuf::from);
        // Harness instances (--ephemeral / --capture) must NEVER run the
        // resume command: a driver FE that runs `claude --continue` spawns a
        // SECOND live instance of the primary FE's claude session inside a
        // hidden window — observed 2026-06-11, where the in-drawer session
        // ended up hosted by its own minimized test driver while the real
        // supervised FE got culled as the "stale" duplicate.
        let harness = cli.ephemeral || cli.capture.is_some();
        let want_terminal_init =
            !harness && (cli.relaunched || settings.terminal_resume_command.is_some());
        let pending_resume_command = if want_terminal_init {
            Some(
                settings
                    .terminal_resume_command
                    .clone()
                    .unwrap_or_else(|| crate::settings::DEFAULT_RESUME_COMMAND.to_string()),
            )
        } else {
            None
        };

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
            code_bg_quad,
            code_border_quad,
            strike_line_quad,
            border_quads: HashMap::new(),
            logo_quad,
            wordmark_quad,
            preview_png,
            preview_png_zoom: 1.0,
            preview_png_pan_px: (0.0, 0.0),
            preview_png_dims: None,
            preview_roi: None,
            preview_png_cache: HashMap::new(),
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
            evt_rx,
            status: "offline · no transport".to_string(),
            nav_prompt: None,
            pending_created_node_id: None,
            pending_deleted_node_id: None,
            notify_sticky_until: None,
            tree: TreeView::new(),
            mode: {
                // CLI override takes precedence; otherwise resume from
                // the persisted last_mode (B5). Default Files.
                let persisted = crate::state_persistence::load();
                if cli.start_mode == "modules" {
                    Mode::Modules
                } else if cli.start_mode == "sessions" {
                    Mode::Sessions
                } else if cli.start_mode == "files" {
                    Mode::Files
                } else {
                    match persisted.last_mode.as_deref() {
                        Some("modules") => Mode::Modules,
                        Some("sessions") => Mode::Sessions,
                        Some("hosts") => Mode::Hosts,
                        _ => Mode::Files,
                    }
                }
            },
            concept_target_fired: None,
            last_cursor_pos: None,
            cursor_moved_at: None,
            concept: None,
            pending_file_edit: None,
            preview_node_id_fired: None,
            driven_preview_hold_cursor: None,
            pending_reveal: None,
            reveal_awaiting: None,
            reveal_refetched: None,
            preview_page: None,
            preview_page_raster_zoom: 1.0,
            preview_page_raster_pending: None,
            preview_reraster_keep_view: false,
            preview_anchor_line: None,
            preview_anchored_to: None,
            pinned_preview_node_id: None,
            tmux_capture_fired_for: None,
            // Restored BL target so the first pty.open re-attaches to
            // wherever the last session left off. None → DEFAULT (sot-llm).
            bl_pane_target: crate::state_persistence::load().last_bl_target,
            // Harness runs (capture or --ephemeral) skip the restore: a
            // persisted workspace switch re-fires tree.root + a root preview
            // after --capture-preview's one-shot, clobbering the captured
            // node with whatever the live session was parked on. Harness
            // runs must be deterministic.
            active_workspace_id: if cli.capture.is_some() || cli.ephemeral {
                None
            } else {
                crate::state_persistence::load().last_workspace_id
            },
            hosts_config: crate::hosts::load(),
            selected_host: crate::state_persistence::load().last_host,
            pending_destroy_target: None,
            host: None,
            daemon_root_basename: None,
            daemon_project_root: None,
            scan_project_root: None,
            last_revision: 0,
            workspace_labels: HashMap::new(),
            workspace_project_roots: HashMap::new(),
            workspace_states: HashMap::new(),
            pending_nav: HashMap::new(),
            prev_workspace_states: HashMap::new(),
            flash_starts: HashMap::new(),
            contrast_dim: cli.contrast_mode == "dim",
            workspace_slugs: Vec::new(),
            default_workspace_slug: None,
            workspace_autostart: HashMap::new(),
            autostarted_sessions: std::collections::HashSet::new(),
            launching_sessions: HashMap::new(),
            pending_autostart: None,
            autostart_scan: None,
            delivery: None,
            deferred_delivery: None,
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
            pending_auto_expand: cli.auto_expand,
            pending_auto_pin: cli.auto_pin,
            pending_demo_function_methods: cli.demo_function_methods.clone(),
            pending_demo_repl_eval: cli.demo_repl_eval.clone(),
            file_parse_retry: std::collections::HashMap::new(),
            pending_start_path: cli.start_path.clone(),
            start_path_fired: None,
            req_tx,
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
            // Seed the terminal at a small placeholder size; first
            // redraw with a real BL content rect resizes it to match.
            // 5000-row scrollback so wheel-up in the LLM pane can walk
            // back through tmux output without forwarding any bytes to
            // tmux itself.
            pty_terminal: vt100::Parser::new(24, 80, 5000),
            pty_size: None,
            repl_scroll: 0,
            preview_scroll: 0,
            md_table_scroll_px: 0.0,
            table_buffers: Vec::new(),
            pty_scroll: 0,
            pane_rects: PaneRects::default(),
            llm_selection: None,
            llm_drag_active: false,
            cursor_px: (0.0, 0.0),
            wheel_residue_y: 0.0,
            last_pty_wheel_at: None,
            maximized: cli.start_maximized,
            bindings: KeyBindings::load_layered(),
            settings,
            upload: None,
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
            // Open straight into the Terminal drawer (so the resume command
            // runs) on a self-relaunch OR any start with a configured
            // resume_command — see `want_terminal_init` above.
            drawer: if cli.start_monitor {
                // `--start-monitor` (capture harness) wins over the
                // resume-command terminal default: an explicit capture ask
                // beats the relaunch convenience, and it also keeps the
                // capture run from spawning the resume command. The
                // subscribe + history prefill for this startup-opened
                // drawer is sent right after construction, where `req_tx`
                // is wired up.
                DrawerContent::Monitor
            } else if want_terminal_init {
                DrawerContent::Terminal
            } else {
                DrawerContent::Closed
            },
            local_term: None,
            term_size: None,
            term_scroll: 0,
            repo_dir,
            pending_resume_command,
            relaunch_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fe_commands: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            fe_state_sig: None,
            focus_on_first_frame: true,
            edit_state: None,
            preview_edit: None,
            help_open: cli.start_help,
            preview_help: None,
            protocol_mismatch: None,
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
            repl_pkg_mode: false,
            history_pos: None,
            history_saved: None,
            text_scale_mult: 1.0,
            preview_src: None,
        };
        // `--demo-sessions a,b:working,c` (capture harness): seed the
        // workspace strip offline so the bottom session strip renders without
        // a live backend. Middle entry is made active so both left + right
        // neighbours show. A `:state` suffix on an entry also seeds
        // `workspace_states[slug] = (state, now)` so its work-state tone
        // renders; a bare slug carries no state (renders as before).
        if !cli.demo_sessions.is_empty() {
            state.workspace_slugs = cli.demo_sessions.clone();
            let now_rfc3339 = chrono::Utc::now().to_rfc3339();
            for (i, s) in cli.demo_sessions.iter().enumerate() {
                state.workspace_labels.insert(s.clone(), s.clone());
                if let Some(Some(st)) = cli.demo_session_states.get(i) {
                    state
                        .workspace_states
                        .insert(s.clone(), (st.clone(), now_rfc3339.clone()));
                    // Mirror into prev so a later live transition off this
                    // seeded state would flash, not first-appear.
                    state.prev_workspace_states.insert(s.clone(), st.clone());
                }
            }
            let mid = cli.demo_sessions.len() / 2;
            state.active_workspace_id = cli.demo_sessions.get(mid).cloned();
        }
        // `--demo-flash a,c` (capture harness): stamp a fresh status-change
        // flash on the listed slugs at startup so a `--capture` shows the
        // flash near full brightness. We also rewrite `prev_workspace_states`
        // to a *different* synthetic state so the same slug reads as a real
        // transition under the live diff path (rather than first-appearance).
        for slug in &cli.demo_flash {
            state
                .flash_starts
                .insert(slug.clone(), std::time::Instant::now());
            // A prior state distinct from whatever was seeded above makes the
            // transition look real; "idle" unless the current seed is idle.
            let prior = match state.workspace_states.get(slug) {
                Some((cur, _)) if cur == "idle" => "working",
                _ => "idle",
            };
            state
                .prev_workspace_states
                .insert(slug.clone(), prior.to_string());
        }
        // `--start-monitor` (capture harness): the drawer was opened at
        // construction; queue the same subscribe + history prefill the
        // Ctrl+M arm sends. The unbounded req channel buffers until the
        // transport connects, so sending here is safe pre-hello.
        if cli.start_monitor {
            let _ = state
                .req_tx
                .send(crate::transport::OutgoingReq::MonitorSubscribe);
            let _ = state
                .req_tx
                .send(crate::transport::OutgoingReq::MonitorHistory {
                    window_s: 300.0,
                    points: 300,
                    until: None,
                    host: None,
                });
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

    /// Drain incoming transport events and apply them. Called at the top of
    /// each redraw — `request_redraw` from the transport task is what makes
    /// drains actually happen.
    /// Dispatch an expand request for the currently-selected row, mirroring
    /// what the Enter/Right keyboard arm does. Returns true when a request
    /// was queued; callers can use that to chain (e.g. `--auto-expand`
    /// consuming itself after the first successful fire).
    fn try_expand_selected(&mut self) -> bool {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return false;
        };
        if !row.node.has_children || row.expanded {
            return false;
        }
        let outgoing = if row.node.kind == "modules" {
            // Modules-mode root re-expansion: re-request `project.scan`
            // for the whole package tree. `tree.children` is files-mode-
            // only and would error on the synthetic "modules:" id; the
            // entire scan ships in one round-trip anyway so per-row
            // expansion doesn't need a separate fetch.
            Some(crate::transport::OutgoingReq::ProjectScan {
                workspace_id: self.active_workspace_id.clone(),
            })
        } else if row.node.kind == "sessions" {
            // Sessions-mode root re-expansion: refresh the workspace
            // registry. Per ADR 0014 the row source is workspace.list,
            // not tmux.list_sessions.
            Some(crate::transport::OutgoingReq::WorkspaceList)
        } else if row.node.kind == "session" {
            // Sessions-mode session row → fetch its panes scoped to this
            // session. apply_children splices them under the session.
            row.node
                .payload
                .get("name")
                .and_then(|v| v.as_str())
                .map(|name| crate::transport::OutgoingReq::TmuxListPanes {
                    session: Some(name.to_string()),
                })
        } else if row.node.kind == "module" {
            row.node
                .payload
                .get("path")
                .and_then(|v| v.as_str())
                .map(|p| crate::transport::OutgoingReq::FileParse {
                    path: p.to_string(),
                    workspace_id: self.active_workspace_id.clone(),
                })
        } else if row.node.kind == "function" {
            // Read module + name out of payload (populated when col-2
            // splice built this row). Functions without that payload
            // came from somewhere else — skip rather than guess.
            let module = row
                .node
                .payload
                .get("module")
                .and_then(|v| v.as_str())
                .map(String::from);
            let name = row
                .node
                .payload
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from);
            let ws = self.active_workspace_id.clone();
            module.zip(name).map(
                |(module, name)| crate::transport::OutgoingReq::FunctionMethods {
                    module,
                    name,
                    workspace_id: ws,
                },
            )
        } else {
            Some(crate::transport::OutgoingReq::TreeChildren {
                parent_id: row.node.id.clone(),
                workspace_id: self.active_workspace_id.clone(),
            })
        };
        if let Some(req) = outgoing {
            if let Err(e) = self.req_tx.send(req) {
                tracing::warn!(error = %e, "drop expand request — channel closed");
                return false;
            }
            return true;
        }
        false
    }

    /// Whether the current preview source is a code shaper (`new_tokens` /
    /// `new_plain`) rather than markdown/image — i.e. one where line anchoring
    /// is meaningful. Keyed off the cached `preview_src` mime.
    fn preview_is_code(&self) -> bool {
        self.preview_src
            .as_ref()
            .map(|(m, _)| {
                m.starts_with("application/vnd.sot.tokens+json")
                    || (m.starts_with("text/") && m != "text/markdown" && m != "text/x-markdown")
            })
            .unwrap_or(false)
    }

    /// If the selected tree row's node id differs from the last one we
    /// asked for a preview of, fire a fresh `preview.get`. The Preview
    /// handler routes the response to the right pane based on mime.
    /// Modules-mode rows (no `files:` prefix) have no backend preview
    /// today; skip them rather than asking and getting an error back.
    /// C2: when a preview is pinned, cursor moves DON'T refresh the
    /// preview — the user is parked on the pinned node and the cursor
    /// is free to roam.
    fn maybe_fire_preview(&mut self) {
        if self.pinned_preview_node_id.is_some() {
            return;
        }
        // `--capture-preview` runs must show the requested node, full stop.
        // Restored nav state (the previous session's cursor) would otherwise
        // auto-fire its own preview and overwrite the captured one — the
        // root-row suppression at the dispatch site doesn't cover a restored
        // non-root cursor.
        if self.capture_preview_armed {
            return;
        }
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        // Files-mode rows are already keyed `files:<relpath>` — fire
        // directly. Modules-mode rows carry an absolute `file` on
        // their payload; we synthesize the `files:<relpath>` id from
        // the cached scan project_root so `preview.get` reuses the
        // same backend codepath.
        let node_id = if row.node.id.starts_with("files:") {
            Some(row.node.id.clone())
        } else if let Some(file) = row.node.payload.get("file").and_then(|v| v.as_str()) {
            if file.is_empty() {
                None
            } else {
                let rel = match &self.scan_project_root {
                    Some(root) if !root.is_empty() => file
                        .strip_prefix(root)
                        .map(|s| s.trim_start_matches(['/', '\\']).to_string())
                        .unwrap_or_else(|| file.to_string()),
                    _ => file.to_string(),
                };
                Some(format!("files:{rel}"))
            }
        } else {
            None
        };
        // Capture the row's definition line (modules-mode rows carry it on
        // `line`) so the Preview reply handler can anchor the code preview to
        // the item. Files-mode rows have no `line` → None → opens at the top.
        let anchor_line = row
            .node
            .payload
            .get("line")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        let Some(id) = node_id else { return };
        // Blink guard: a freshly *driven* preview (fe-command / nav.preview) that
        // targeted a node not in the tree left the cursor parked on `id`'s row.
        // Don't fire `id`'s preview over the driven one while the cursor is still
        // parked there — that's the deep-path blink. The hold lifts as soon as
        // the cursor moves to a different row (then normal follow resumes).
        if let Some(held) = self.driven_preview_hold_cursor.clone() {
            if held == id {
                return;
            }
            self.driven_preview_hold_cursor = None;
        }
        if self.preview_node_id_fired.as_ref() == Some(&id) {
            // Same file already shown — no re-fetch needed. But items within a
            // module share a file, so the cursor moving between them must still
            // re-anchor the (already rendered) code preview to the new item.
            // Guard on a real change in target line so we don't re-anchor every
            // redraw and fight the user's manual scroll on a stable selection.
            if anchor_line != self.preview_anchored_to {
                if let Some(line) = anchor_line {
                    if line > 0 && self.preview_is_code() {
                        self.preview_scroll =
                            self.preview_md.anchor_scroll_for_def_line(line as usize);
                        self.window.request_redraw();
                    }
                }
                self.preview_anchored_to = anchor_line;
            }
            return;
        }
        // New target: drop any in-flight zoom re-raster so its reply can't
        // be mistaken for this fetch.
        self.preview_page_raster_pending = None;
        let (fit_w, fit_h) = self.preview_fit_px();
        if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::PreviewGet {
            node_id: id.clone(),
            workspace_id: self.active_workspace_id.clone(),
            // Cursor-driven fetch always opens at page 1; the reply's
            // extras re-seed `preview_page` for the n/p transport.
            page: None,
            fit_w,
            fit_h,
        }) {
            tracing::warn!(error = %e, %id, "drop preview.get request — channel closed");
            return;
        }
        self.preview_node_id_fired = Some(id);
        self.preview_anchor_line = anchor_line;
    }

    /// Badge-floor entry point (ADR 0025 §1). Records that a `nav.preview`
    /// result for workspace `ws` (workspace-relative `path`) arrived while the
    /// FE was viewing a *different* workspace, and surfaces it non-disruptively:
    /// the workspace's nav row + bottom-strip name badge "result pending", and
    /// the status line says where the result is waiting. The view is NEVER
    /// switched here — the user keeps their place; the pending preview is driven
    /// only when they later switch to `ws` (see `switch_to_workspace`). This is
    /// the floor's contract: a result always reaches the user, never silently
    /// dropped. Latest-wins per workspace. The future `op::FE_COMMAND` handler
    /// will reuse this method.
    fn mark_pending_nav(&mut self, ws: String, path: String) {
        self.status = pending_nav_status(&ws, &path);
        self.pending_nav.insert(ws, path);
        self.window.request_redraw();
    }

    /// Item 2: a session drove a `nav.preview` envelope at us. Act ONLY when
    /// it targets our currently-active workspace (the gate the maintainer and I locked —
    /// a broadcast reaches every FE, so each acts only for the workspace it's
    /// viewing; others ignore it, and it's NEVER rendered as chat either way).
    /// On a match: switch to Files mode and fire `preview.get` for
    /// `files:<path>`. node ids are workspace-relative and the backend
    /// resolves them directly, so no tree expansion is required to show the
    /// file. (Cursor-reveal — expanding the tree to select the row — is a
    /// follow-up; the preview pane is the payload of `nav.preview`.)
    fn handle_nav_envelope(&mut self, env: &NavEnvelope) {
        let current = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone());
        if current.as_deref() != Some(env.workspace.as_str()) {
            // Badge floor (ADR 0025 §1): the result targets a workspace we're
            // not viewing. Don't silently drop it — record + badge it so it
            // reaches the user when they switch to that workspace.
            tracing::debug!(target_ws = %env.workspace, ?current,
                "nav.preview targets a non-active workspace — badging as pending (not chat)");
            self.mark_pending_nav(env.workspace.clone(), env.path.clone());
            return;
        }
        // Drive both panes via the shared same-ws open: preview body now, and a
        // deep-path cursor reveal so the cursor follows the file even when its
        // ancestor dirs aren't expanded yet. Without the reveal the header kept
        // labelling the OLD cursor node while the body showed the driven file —
        // the header/body mismatch the maintainer hit (preview · circles.png over
        // KNOWLEDGE_BASE.md content).
        self.drive_same_ws_open(&env.path);
        self.status = format!("nav ← agent · {}", env.path);
        self.window.request_redraw();
        tracing::info!(node_id = %format!("files:{}", env.path), ws = %env.workspace,
            "nav.preview driven by agent");
    }

    /// Same-workspace driven open (ADR 0025): show `path` (workspace-relative)
    /// in the Files-mode preview AND move the nav cursor onto its row. This is
    /// the single entry point both `fe.command` (`preview`/`reveal`) and the
    /// `nav.preview` relay use for the same-ws case, so the BE never has to
    /// issue a separate cursor move — one command drives both panes.
    ///
    /// The preview body fires immediately for instant feedback. The cursor
    /// lands now if the row is already visible; otherwise `pending_reveal` is
    /// armed and `drive_reveal_step` expands ancestor dirs asynchronously until
    /// the row materializes (the deep-path case the old code left body-only).
    fn drive_same_ws_open(&mut self, path: &str) {
        self.mode = Mode::Files;
        let node_id = format!("files:{path}");
        // Fire the preview body up front — don't wait on tree expansion.
        let (fit_w, fit_h) = self.preview_fit_px();
        if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::PreviewGet {
            node_id: node_id.clone(),
            workspace_id: self.active_workspace_id.clone(),
            page: None,
            fit_w,
            fit_h,
        }) {
            tracing::warn!(error = %e, %node_id,
                "drive_same_ws_open: drop preview.get — channel closed");
            return;
        }
        self.preview_node_id_fired = Some(node_id.clone());
        self.preview_anchor_line = None;
        // Cursor reveal: land now if the row is already in the tree; else expand
        // ancestors asynchronously.
        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == node_id) {
            self.tree.selected = idx;
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            self.driven_preview_hold_cursor = None;
        } else {
            // Hold the per-frame preview-follow off the (stale) cursor row so it
            // doesn't clobber the driven preview while ancestors expand; the
            // hold lifts when the cursor lands on the target.
            self.driven_preview_hold_cursor = self
                .tree
                .rows
                .get(self.tree.selected)
                .map(|r| r.node.id.clone());
            self.pending_reveal = Some(node_id);
            self.reveal_awaiting = None;
            self.drive_reveal_step();
        }
        self.window.request_redraw();
    }

    /// Advance an in-flight deep-path reveal (`pending_reveal`). No-op when no
    /// reveal is armed, so it's safe to call unconditionally after every
    /// `tree.children` splice. When the target row is now visible it lands the
    /// cursor and clears the reveal; otherwise it expands the deepest visible
    /// ancestor dir (one `tree.children` request) and waits for the reply to
    /// re-enter here. Self-terminating: if the deepest visible ancestor is
    /// already expanded yet the target still isn't present, the path doesn't
    /// resolve and the reveal is dropped (the preview body already showed).
    fn drive_reveal_step(&mut self) {
        let Some(target_id) = self.pending_reveal.clone() else {
            return;
        };
        // Target row visible now → land the cursor and finish.
        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == target_id) {
            self.tree.selected = idx;
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            self.reveal_refetched = None;
            self.driven_preview_hold_cursor = None;
            // Re-anchor the header/preview onto the landed row. The body was
            // already fetched (preview_node_id_fired == target_id), so this
            // doesn't re-fetch — it just keeps header + body in sync.
            self.maybe_fire_preview();
            self.window.request_redraw();
            tracing::info!(%target_id, "reveal: landed cursor on driven-open target");
            return;
        }
        let Some(rel) = target_id.strip_prefix("files:") else {
            self.pending_reveal = None;
            self.reveal_awaiting = None;
            return;
        };
        // Expand the deepest ancestor that's present but not yet expanded
        // (deepest-first ordering from `ancestor_rels`).
        for anc in ancestor_rels(rel) {
            let anc_id = format!("files:{anc}");
            let Some(row) = self.tree.rows.iter().find(|r| r.node.id == anc_id) else {
                continue;
            };
            if row.expanded {
                // Deepest visible ancestor is expanded but the target isn't
                // among its cached children. For a brand-new file (an agent
                // wrote it after this dir was last listed) the cache is simply
                // stale: `list_dir` does a fresh stat, so re-fetching this dir's
                // children ONCE surfaces the file, `apply_children` inserts it,
                // and the re-entrant `drive_reveal_step` lands the cursor — one
                // loopback round-trip, sub-second. Covers directed preview,
                // reveal, AND badge-consume (all funnel through here), so
                // generate→preview and badge→navigate work on fresh files.
                let key = (target_id.clone(), anc_id.clone());
                if self.reveal_refetched.as_ref() == Some(&key) {
                    // Already force-refreshed this dir for this target and it's
                    // STILL absent → genuinely gone. Stop; the body preview stands.
                    tracing::debug!(%target_id, %anc_id,
                        "reveal: re-fetched expanded ancestor, target still absent — stopping");
                    self.pending_reveal = None;
                    self.reveal_awaiting = None;
                    self.reveal_refetched = None;
                    return;
                }
                if let Err(e) = self
                    .req_tx
                    .send(crate::transport::OutgoingReq::TreeChildren {
                        parent_id: anc_id.clone(),
                        workspace_id: self.active_workspace_id.clone(),
                    })
                {
                    tracing::warn!(error = %e, %anc_id,
                        "reveal: drop tree.children refresh — channel closed");
                    self.pending_reveal = None;
                    self.reveal_awaiting = None;
                    self.reveal_refetched = None;
                    return;
                }
                self.reveal_refetched = Some(key);
                tracing::info!(%target_id, %anc_id,
                    "reveal: force-refresh expanded ancestor for a fresh (not-yet-listed) file");
                return;
            }
            if !row.node.has_children {
                self.pending_reveal = None;
                self.reveal_awaiting = None;
                return;
            }
            // Already requested this exact level → keep waiting (don't storm).
            if self.reveal_awaiting.as_deref() == Some(anc_id.as_str()) {
                return;
            }
            if let Err(e) = self
                .req_tx
                .send(crate::transport::OutgoingReq::TreeChildren {
                    parent_id: anc_id.clone(),
                    workspace_id: self.active_workspace_id.clone(),
                })
            {
                tracing::warn!(error = %e, %anc_id,
                    "reveal: drop tree.children — channel closed");
                self.pending_reveal = None;
                self.reveal_awaiting = None;
                return;
            }
            self.reveal_awaiting = Some(anc_id);
            return;
        }
        // No ancestor visible at all (root collapsed, or a root-level file the
        // tree hasn't loaded). Nothing to expand toward — drop the reveal; the
        // preview body already showed.
        tracing::debug!(%target_id, "reveal: no visible ancestor to expand — stopping");
        self.pending_reveal = None;
        self.reveal_awaiting = None;
    }

    /// C2 pin-and-leave toggle. If a preview is pinned, `p` (from any row)
    /// UNPINS it and jumps the cursor to the formerly-pinned row, so the user
    /// lands back on it instead of having to hunt the tree for the pinned node
    /// to clear it. If nothing is pinned, `p` pins the cursor row and the
    /// preview then stays put as the cursor roams. Pinning is `files:`-only —
    /// modules/sessions rows have no backend preview, so pinning them would be
    /// a confusing no-op.
    fn toggle_pin(&mut self) {
        // Pinned → unpin from anywhere, and move the cursor to the pinned node
        // if it's in the current tree (so the user sees what was pinned).
        if let Some(pinned) = self.pinned_preview_node_id.take() {
            if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == pinned) {
                self.tree.selected = idx;
            }
            self.status = format!("unpinned · {pinned}");
            self.window.request_redraw();
            return;
        }
        // Nothing pinned → pin the cursor row (files: only).
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        let id = row.node.id.clone();
        if !id.starts_with("files:") {
            return;
        }
        self.pinned_preview_node_id = Some(id.clone());
        self.status = format!("pinned · {id}");
        self.window.request_redraw();
    }

    /// B5: persist (last_mode, last_bl_target) so a fresh launch resumes
    /// where the user left off. Called from each Mode-switch and each
    /// attach_session_to_bl; not from every cursor move (those don't
    /// reflect coarse resume state).
    fn persist_resume_state(&self) {
        // Harness instances never write the per-host shared state (B8
        // single-writer rule) — a driver/capture FE would clobber the
        // primary FE's resume state.
        if self.ephemeral {
            return;
        }
        // Snapshot window geometry in *logical* pixels so the next
        // launch's `with_inner_size` / `with_position` lands cleanly
        // regardless of the current monitor's DPR.
        let scale = self.window.scale_factor();
        let inner = self.window.inner_size().to_logical::<f64>(scale);
        let pos = self
            .window
            .outer_position()
            .ok()
            .map(|p| p.to_logical::<f64>(scale));
        let s = crate::state_persistence::GlobalState {
            last_mode: Some(self.mode.label().to_string()),
            last_bl_target: self.bl_pane_target.clone(),
            last_workspace_id: self.active_workspace_id.clone(),
            last_host: self.selected_host.clone(),
            window_w: Some(inner.width),
            window_h: Some(inner.height),
            window_x: pos.as_ref().map(|p| p.x),
            window_y: pos.as_ref().map(|p| p.y),
            fullscreen: Some(self.window.fullscreen().is_some()),
            font_scale: Some(self.text_scale_mult as f64),
            nav_selected_id: self
                .tree
                .rows
                .get(self.tree.selected)
                .map(|r| r.node.id.clone()),
            nav_scroll: Some(self.tree_scroll),
        };
        crate::state_persistence::save(&s);
    }

    /// Key used to index `workspace_ui_snapshots` for the *current*
    /// workspace. The daemon's default workspace doesn't carry a slug
    /// in `active_workspace_id`; `<default>` is the literal we use
    /// instead so it has a snapshot slot too.
    fn current_workspace_key(&self) -> String {
        self.active_workspace_id
            .clone()
            .unwrap_or_else(|| "<default>".to_string())
    }

    /// Cycle to the next or previous workspace in `workspace_slugs`
    /// order. `direction = +1` walks forward (Shift+Right), `-1` walks
    /// backward (Shift+Left). Wraps at both ends. Resolves the *current*
    /// position via `active_workspace_id`, falling back to
    /// `default_workspace_slug` for "we're on the default workspace".
    /// No-op until `workspace.list` has populated the cache, and a
    /// no-op when there's only one workspace registered (nothing to
    /// cycle to). Routes through `switch_to_workspace` so all the
    /// snapshot / repaint / BL-retarget machinery fires the same way
    /// it does for Sessions-Enter.
    fn cycle_workspace(&mut self, direction: i32) {
        if self.workspace_slugs.len() < 2 {
            return;
        }
        let current = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone());
        let n = self.workspace_slugs.len() as i32;
        let idx = current
            .as_deref()
            .and_then(|s| self.workspace_slugs.iter().position(|x| x == s))
            .map(|p| p as i32)
            .unwrap_or(0);
        let next = ((idx + direction).rem_euclid(n)) as usize;
        let next_slug = self.workspace_slugs[next].clone();
        let tmux_session = format!("sot-be-{next_slug}");
        // Flick the brand wheels in the direction of travel (forward = CW). The
        // per-frame decay + redraw live in the bottom-strip block; nudge the
        // event loop so the spin animates even if nothing else is dirty.
        self.wheel_vel = (self.wheel_vel + direction as f32 * WHEEL_FLICK_VEL)
            .clamp(-WHEEL_MAX_VEL, WHEEL_MAX_VEL);
        self.dirty = true;
        self.window.request_redraw();
        self.switch_to_workspace(Some(next_slug), Some(tmux_session));
    }

    /// Switch the nav mode and fire that mode's data fetch. Shared by the
    /// f/m/s/h keybinds and the ADR-0019 `mode` command; no-op if already in
    /// `mode`.
    fn enter_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            Mode::Files => {
                if let Err(e) = self.req_tx.send(OutgoingReq::TreeRoot {
                    mode: "files".to_string(),
                    workspace_id: self.active_workspace_id.clone(),
                }) {
                    tracing::warn!(error = %e, "drop tree.root request — channel closed");
                }
            }
            Mode::Modules => {
                if let Err(e) = self.req_tx.send(OutgoingReq::ProjectScan {
                    workspace_id: self.active_workspace_id.clone(),
                }) {
                    tracing::warn!(error = %e, "drop project.scan request — channel closed");
                }
            }
            Mode::Sessions => {
                self.tmux_capture_fired_for = None;
                if let Err(e) = self.req_tx.send(OutgoingReq::WorkspaceList) {
                    tracing::warn!(error = %e, "drop workspace.list request — channel closed");
                }
            }
            Mode::Hosts => {
                self.populate_hosts_tree();
            }
        }
        self.persist_resume_state();
    }

    /// Toggle the backend's Files-mode "show hidden files" flag for the active
    /// workspace (the `.` keybind → `nav.toggle_hidden`), then re-fetch the
    /// files tree root so the change is visible now. The two requests ride the
    /// same ordered connection, so the backend flips the flag before it serves
    /// the tree.root. Gated to nav focus + Files mode at the call site; the
    /// tree.root reply is dropped in non-Files modes anyway. Toggling collapses
    /// the tree to its root — deeper dirs pick up the new visibility on their
    /// next expand (tree.children reads the same flag).
    fn toggle_hidden_files(&mut self) {
        if let Err(e) = self.req_tx.send(OutgoingReq::ToggleHidden {
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, "drop nav.toggle_hidden — channel closed");
            return;
        }
        if matches!(self.mode, Mode::Files) {
            if let Err(e) = self.req_tx.send(OutgoingReq::TreeRoot {
                mode: "files".to_string(),
                workspace_id: self.active_workspace_id.clone(),
            }) {
                tracing::warn!(error = %e, "drop tree.root after nav.toggle_hidden — channel closed");
            }
        }
    }

    /// Drain commands the FE-control watcher (ADR 0019) enqueued and dispatch
    /// each on the main thread. Called near the top of `window_event` after a
    /// `request_redraw` wake; a cheap no-op when the queue is empty. The lock
    /// is scoped to the drain so dispatch (which mutates `self`) doesn't hold
    /// it.
    fn drain_fe_commands(&mut self) {
        let cmds: Vec<FeCommand> = match self.fe_commands.lock() {
            Ok(mut q) => q.drain(..).collect(),
            Err(_) => return,
        };
        for cmd in cmds {
            self.dispatch_fe_command(cmd);
        }
    }

    /// Live-refresh one directory's listing in the Files nav tree by re-fetching
    /// its `tree.children` (the reply runs `apply_children`, which *replaces*
    /// that dir's rows). Used by the file-watcher (`preview.changed`) path so a
    /// create/remove on disk shows up without a manual re-nav — mirrors the
    /// post-create/post-delete refresh the Ctrl+N / Ctrl+D flows already do.
    ///
    /// Guarded so a watcher event never *surprise-expands* a folder: only fires
    /// when `dir_id` is an already-expanded row in the current Files tree. A
    /// no-op outside Files mode, or when the dir isn't shown (collapsed / not
    /// expanded / a different workspace's path), in which case the reply's
    /// `apply_children` would ignore the unknown parent anyway.
    fn refresh_tree_dir_if_expanded(&mut self, dir_id: &str) {
        if self.mode != Mode::Files {
            return;
        }
        let shown_expanded = self
            .tree
            .rows
            .iter()
            .any(|r| r.node.id == dir_id && r.expanded);
        if !shown_expanded {
            return;
        }
        if let Err(e) = self
            .req_tx
            .send(crate::transport::OutgoingReq::TreeChildren {
                parent_id: dir_id.to_string(),
                workspace_id: self.active_workspace_id.clone(),
            })
        {
            tracing::warn!(error = %e, %dir_id, "drop tree.children (watcher refresh)");
        }
    }

    /// Apply one FE-control command, reusing the methods the keybinds call so
    /// commands inherit the same routing (incl. the ADR-0014 per-workspace
    /// tree-reply guard).
    fn dispatch_fe_command(&mut self, cmd: FeCommand) {
        match cmd {
            FeCommand::Workspace { slug, boot } => {
                // null/empty/"default"/"<default>" → the daemon-default
                // workspace (active_workspace_id = None, keep current BL).
                let slug = slug.filter(|s| !s.is_empty() && s != "default" && s != "<default>");
                if let Some(s) = slug.as_deref() {
                    if !self.workspace_slugs.iter().any(|x| x == s) {
                        tracing::warn!(slug = %s, "fe-command workspace: unknown slug, ignoring");
                        return;
                    }
                }
                let tmux = slug.as_ref().map(|s| format!("sot-be-{s}"));
                // `--boot` (scriptable spawn->goto->boot): seed the target's
                // autostart flag so the attach below arms ccb — unconditional of
                // what workspace.list reported, fixing the registry-flag timing
                // where a freshly-spawned ws hadn't been flagged yet. The
                // existing already-running guards (autostarted_sessions /
                // pane_command==claude) still prevent prompt-spam into a live
                // agent, so this only boots a pane that ISN'T already running
                // claude. No-op for the daemon-default (no tmux target to boot).
                if boot {
                    if let Some(t) = tmux.as_ref() {
                        let e = self
                            .workspace_autostart
                            .entry(t.clone())
                            .or_insert_with(|| WsAutostart {
                                autostart_claude: true,
                                agent_name: String::new(),
                                task: String::new(),
                            });
                        e.autostart_claude = true;
                    }
                }
                tracing::info!(?slug, boot, "fe-command: switch workspace");
                self.switch_to_workspace(slug, tmux);
            }
            FeCommand::CycleWs { dir } => {
                let dir = if dir == 0 { 1 } else { dir };
                tracing::info!(dir, "fe-command: cycle workspace");
                self.cycle_workspace(dir);
            }
            FeCommand::ReloadKeybindings => {
                self.bindings = KeyBindings::load_layered();
                tracing::info!("fe-command: reloaded keybindings");
                self.status = "keybindings reloaded".to_string();
                self.window.request_redraw();
            }
            FeCommand::Notify { text, level } => {
                tracing::info!(?level, %text, "fe-command: notify");
                self.status = text;
                // Pin it briefly so a workspace switch doesn't instantly rebuild
                // the status line over it (see NOTIFY_STICKY).
                self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                self.window.request_redraw();
            }
            FeCommand::OpenUrl { url } => {
                // Scheme already allowlisted (http/https) at route time.
                tracing::info!(%url, "fe-command: open_url");
                match open_url_in_browser(&url) {
                    Ok(()) => self.status = format!("opened in browser · {url}"),
                    Err(e) => self.status = format!("open_url failed · {e}"),
                }
                self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                self.window.request_redraw();
            }
            FeCommand::Mode { mode } => {
                let m = match mode.as_str() {
                    "files" => Some(Mode::Files),
                    "modules" => Some(Mode::Modules),
                    "sessions" => Some(Mode::Sessions),
                    "hosts" => Some(Mode::Hosts),
                    _ => None,
                };
                match m {
                    Some(m) => {
                        tracing::info!(%mode, "fe-command: mode");
                        self.enter_mode(m);
                    }
                    None => {
                        tracing::warn!(%mode, "fe-command mode: unknown mode, ignoring");
                    }
                }
            }
            FeCommand::Nav { action } => {
                tracing::info!(%action, "fe-command: nav");
                match action.as_str() {
                    "down" => self.tree.move_down(),
                    "up" => self.tree.move_up(),
                    "expand" => {
                        self.try_expand_selected();
                    }
                    "collapse" => {
                        if !self.tree.collapse_selected() {
                            if let Some(p) = self.tree.parent_of_selected() {
                                self.tree.selected = p;
                            }
                        }
                    }
                    "pin" => self.toggle_pin(),
                    other => {
                        tracing::warn!(action = %other, "fe-command nav: unknown action, ignoring");
                        return;
                    }
                }
                self.maybe_fire_preview();
                self.window.request_redraw();
            }
            FeCommand::CaptureRoi => {
                tracing::info!("fe-command: capture_roi");
                self.capture_roi();
            }
            FeCommand::Preview {
                workspace,
                path,
                urgent,
            } => {
                // Same-ws short-circuit: if the target workspace is the one we're
                // already viewing, render in place NOW (mirrors handle_nav_envelope
                // gpu.rs:3399+, the in-place branch the imperative path dropped).
                // Without this, a same-ws preview badges + waits for a switch that
                // never comes (you're already there) — so a naive `sot-fe
                // preview <active-ws> <file>` opened nothing. The decision is a
                // pure fn (`preview_targets_active_ws`) so it's unit-tested.
                if preview_targets_active_ws(
                    self.active_workspace_id.as_deref(),
                    self.default_workspace_slug.as_deref(),
                    &workspace,
                ) {
                    tracing::info!(%workspace, %path, "fe-command: preview (same-ws, render in place)");
                    // Drive both panes: preview body + deep-path cursor reveal.
                    // The cursor follows the preview even when the file's
                    // ancestor dirs aren't expanded yet, so the nav header +
                    // viewport stay in sync (no more body-only / header mismatch).
                    self.drive_same_ws_open(&path);
                    return;
                }
                // Cross-workspace preview: badge by default, NEVER steal the
                // user's session (maintainer clarification 2026-07-10 PM,
                // revising the same morning's directive after living with
                // always-switch: "always set the nav and show means the file
                // should be selected in the nav and shown in preview, NOT to
                // yank my session over... I don't want to be yanked over mid
                // sentence"). The morning's actual bug was completeness — the
                // on-switch consume wasn't landing the nav cursor — which the
                // pending-nav reveal (#4 fix, switch_to_workspace) now does:
                // when the user visits the badged workspace, the file is
                // cursored in the nav AND rendered in the preview, always.
                //
                // `urgent` is the explicit user-requested "capture session
                // focus" option (sot-fe --urgent --fe <handle>): the route
                // layer only honors it on a DIRECTED send (broadcast urgent is
                // stripped — route_preview_urgent_is_directed_only), so a
                // blanket agent broadcast can never force-switch the view.
                if urgent {
                    tracing::info!(%workspace, %path, "fe-command: preview (user-requested focus capture)");
                    self.mark_pending_nav(workspace.clone(), path.clone());
                    let is_default =
                        workspace.is_empty() || workspace == "default" || workspace == "<default>";
                    let (slug, tmux) = if is_default {
                        (None, None)
                    } else {
                        (Some(workspace.clone()), Some(format!("sot-be-{workspace}")))
                    };
                    self.switch_to_workspace(slug, tmux);
                } else {
                    // Badge floor: record + badge; the pending preview (body +
                    // nav-cursor reveal) is driven when the user next switches
                    // to `workspace` (see `switch_to_workspace`).
                    tracing::info!(%workspace, %path, "fe-command: preview (badge)");
                    self.mark_pending_nav(workspace, path);
                }
            }
            FeCommand::Reveal {
                workspace,
                path,
                urgent,
            } => {
                // reveal == preview: the same-ws preview path now performs the
                // deep tree-expand-and-select (cursor follows the file, ancestor
                // dirs expand async), so `reveal` and `preview` both drive the
                // nav cursor onto the file — the BE need not pick the right verb
                // or issue a separate cursor move. The cross-ws force-show/badge
                // semantics are shared too.
                self.dispatch_fe_command(FeCommand::Preview {
                    workspace,
                    path,
                    urgent,
                });
            }
        }
    }

    /// Write `fe-state.json` (ADR 0019) when the observable state changed
    /// since the last write. Called from the redraw path — a cheap signature
    /// check makes it a no-op on unchanged frames, so the readback file only
    /// touches disk when active workspace / mode / focus / slugs / rev move.
    fn maybe_write_fe_state(&mut self) {
        // B8 single-writer rule: only the primary FE owns fe-state.json.
        if self.ephemeral {
            return;
        }
        let Some(path) = fe_state_path() else {
            return;
        };
        let mode = match self.mode {
            Mode::Files => "files",
            Mode::Modules => "modules",
            Mode::Sessions => "sessions",
            Mode::Hosts => "hosts",
        };
        let focus = match self.focus {
            PaneFocus::NavTree => "nav",
            PaneFocus::Preview => "preview",
            PaneFocus::Llm => "llm",
            PaneFocus::Repl => "repl",
        };
        let active = self.active_workspace_id.clone();
        let host = self.host.clone();
        let rev = self.last_revision;
        let workspaces = self.workspace_slugs.clone();
        // ADR 0022: the active image preview, so the LLM pane knows what the
        // user is zoomed into. The signature buckets zoom (0.1) + ROI origin/
        // size (32 px) so a continuous pan/zoom gesture rewrites at a coarse
        // cadence instead of every frame; the body carries the exact ROI.
        let preview_sig: Option<(String, u32, u32, u32, u32, u32)> =
            self.preview_roi.as_ref().map(|r| {
                (
                    r.node_id.clone(),
                    (r.zoom * 10.0) as u32,
                    r.x >> 5,
                    r.y >> 5,
                    r.w >> 5,
                    r.h >> 5,
                )
            });
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        mode.hash(&mut hasher);
        focus.hash(&mut hasher);
        active.hash(&mut hasher);
        host.hash(&mut hasher);
        rev.hash(&mut hasher);
        workspaces.hash(&mut hasher);
        preview_sig.hash(&mut hasher);
        let sig = hasher.finish();
        if self.fe_state_sig == Some(sig) {
            return;
        }
        let preview = match &self.preview_roi {
            Some(r) => serde_json::json!({
                "node_id": r.node_id,
                "path": r.path,
                "dims": [r.src_w, r.src_h],
                "zoom": r.zoom,
                "roi": { "x": r.x, "y": r.y, "w": r.w, "h": r.h },
            }),
            None => serde_json::Value::Null,
        };
        let json = serde_json::json!({
            "rev": rev,
            "active_workspace": active,
            "workspaces": workspaces,
            "mode": mode,
            "focus": focus,
            "host": host,
            "preview": preview,
        });
        let body = match serde_json::to_vec(&json) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "serialize fe-state failed");
                return;
            }
        };
        // Atomic temp+rename so a reader never sees a half-written file.
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)) {
            tracing::warn!(error = %e, "write fe-state.json failed");
            return;
        }
        self.fe_state_sig = Some(sig);
    }

    /// Rebuild the nav tree from `hosts_config`. Called on entering
    /// `Mode::Hosts`. Each `[host.<name>]` section in `hosts.toml`
    /// becomes one row; the currently-selected host (per state-toml's
    /// `last_host`) cursor-lands by default, otherwise row 0.
    fn populate_hosts_tree(&mut self) {
        let root = TreeNode {
            id: "hosts:".to_string(),
            label: "hosts".to_string(),
            kind: "hosts".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        let selected_now = self
            .selected_host
            .clone()
            .or_else(|| self.hosts_config.default_host.clone());
        let children: Vec<TreeNode> = self
            .hosts_config
            .hosts
            .iter()
            .map(|h| {
                let mut badges = Vec::new();
                if Some(h.name.as_str()) == selected_now.as_deref() {
                    badges.push("current".to_string());
                }
                if self.hosts_config.default_host.as_deref() == Some(h.name.as_str()) {
                    badges.push("default".to_string());
                }
                let endpoint = if let Some(port) = h.tcp_port {
                    let local = if let Some(alias) = h.ssh_alias.as_deref() {
                        format!("{alias}:{port}")
                    } else {
                        format!("127.0.0.1:{port}")
                    };
                    if let Some(sock) = h.remote_socket.as_deref() {
                        format!("{local} -> {sock}")
                    } else {
                        local
                    }
                } else if let Some(sock) = h.socket.as_deref() {
                    sock.to_string()
                } else {
                    "(no endpoint)".to_string()
                };
                let label = format!("{} · {}", h.name, endpoint);
                let mut payload = serde_json::Map::new();
                payload.insert(
                    "name".to_string(),
                    serde_json::Value::String(h.name.clone()),
                );
                TreeNode {
                    id: format!("hosts:{}", h.name),
                    label,
                    kind: "host".to_string(),
                    has_children: false,
                    badges,
                    payload,
                }
            })
            .collect();
        // Default-cursor on the currently-selected host so Enter on a
        // fresh `h`-press confirms the current value rather than the
        // first row by accident.
        let cursor = selected_now
            .as_deref()
            .and_then(|n| self.hosts_config.hosts.iter().position(|h| h.name == n))
            .unwrap_or(0);
        self.tree.set_root(root, children);
        if self.tree.rows.len() > cursor {
            self.tree.selected = cursor;
        }
        self.window.request_redraw();
    }

    /// Mode::Hosts Enter handler — persist the cursor row's host slug
    /// to `state-<hostname>.toml::last_host`. Doesn't restart anything
    /// live; surfaces a status message telling the user to Ctrl+Q +
    /// relaunch to apply.
    fn pick_host_under_cursor(&mut self) {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        if row.node.kind != "host" {
            return;
        }
        let Some(name) = row
            .node
            .payload
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            return;
        };
        self.selected_host = Some(name.clone());
        self.persist_resume_state();
        self.status = format!("host = {name} · Ctrl+Q + relaunch to apply");
        self.populate_hosts_tree();
    }

    /// Rebuild the chrome status line to reflect the currently active
    /// workspace. Format: `connected · <host>:<workspace_label> · rev N`.
    /// Falls back to the slug if `workspace_labels` hasn't been populated
    /// yet, and to the daemon's project_root basename for the default
    /// workspace. No-op until the hello response has landed (no host).
    fn rebuild_connection_status(&mut self) {
        // Don't clobber a fresh notify toast: hold it on the status line until
        // its sticky window elapses (a workspace switch would otherwise rebuild
        // over it immediately). Once elapsed, clear the flag and rebuild.
        if let Some(until) = self.notify_sticky_until {
            if std::time::Instant::now() < until {
                return;
            }
            self.notify_sticky_until = None;
        }
        let Some(host) = self.host.clone() else {
            return;
        };
        let label = self
            .active_workspace_label()
            .unwrap_or_else(|| "default".to_string());
        self.status = format!("connected · {host}:{label} · rev {}", self.last_revision);
    }

    /// Display label for the active workspace: the per-workspace name from
    /// `workspace.list` (falling back to the slug), or the daemon's
    /// project_root basename for the default workspace. This is also the
    /// basename of the Files-mode root directory, so it doubles as the
    /// expected nav root-row label (used to reconcile a stale root after a
    /// snapshot restore). `None` before the hello response has landed.
    fn active_workspace_label(&self) -> Option<String> {
        match self.active_workspace_id.as_deref() {
            Some(slug) => Some(
                self.workspace_labels
                    .get(slug)
                    .cloned()
                    .unwrap_or_else(|| slug.to_string()),
            ),
            None => self.daemon_root_basename.clone(),
        }
    }

    /// Capture the chrome's current view state into the per-workspace
    /// snapshot map. Called immediately before changing
    /// `active_workspace_id` so the workspace we're leaving keeps every
    /// mode-bearing UI bit: nav tree + scroll + focus, preview source
    /// for repaint, concept slot, drift bookkeeping, pty target.
    fn snapshot_current_workspace_ui(&mut self) {
        let key = self.current_workspace_key();
        self.workspace_ui_snapshots.insert(
            key,
            WorkspaceUiSnapshot {
                mode: self.mode,
                tree: self.tree.clone(),
                tree_scroll: self.tree_scroll,
                bl_pane_target: self.bl_pane_target.clone(),
                preview_node_id_fired: self.preview_node_id_fired.clone(),
                pinned_preview_node_id: self.pinned_preview_node_id.clone(),
                preview_src: self.preview_src.clone(),
                concept: self.concept.clone(),
                file_ast_hashes: self.file_ast_hashes.clone(),
                file_parse_fired: self.file_parse_fired.clone(),
                tmux_capture_fired_for: self.tmux_capture_fired_for.clone(),
                edit_state: self.edit_state.clone(),
            },
        );
    }

    /// If a snapshot exists for the workspace keyed by `key`, restore
    /// the chrome to it and return `true`. Caller skips the `tree.root`
    /// re-fetch in that case and the preview repaints from the cached
    /// source. Returns `false` if there's no prior state for this
    /// workspace — caller falls back to fetching fresh.
    fn restore_workspace_ui(&mut self, key: &str) -> bool {
        let Some(snap) = self.workspace_ui_snapshots.get(key).cloned() else {
            return false;
        };
        self.mode = snap.mode;
        self.tree = snap.tree;
        // Reconcile the Files-mode root label to the active workspace. The
        // snapshot restores the tree wholesale and (by design, for instant
        // switch-back) skips the `tree.root` re-fetch — so a root row that was
        // captured with a stale label survives. The Files root id is the
        // workspace-independent "files:", and its label is the project-dir
        // basename, which equals the workspace label; patch it directly here
        // rather than re-fetching (which would collapse the restored tree).
        // Files mode only: other modes root on a different entity (package
        // name, session list) whose label isn't the workspace name.
        if self.mode == Mode::Files {
            if let Some(label) = self.active_workspace_label() {
                if let Some(root) = self.tree.rows.first_mut() {
                    if root.node.id == "files:" {
                        root.node.label = label;
                    }
                }
            }
        }
        self.tree_scroll = snap.tree_scroll;
        // focus is global across workspaces — don't restore. The user
        // expects pane focus to follow their last interaction regardless
        // of which workspace is active.
        self.bl_pane_target = snap.bl_pane_target;
        self.preview_node_id_fired = snap.preview_node_id_fired;
        self.pinned_preview_node_id = snap.pinned_preview_node_id;
        // preview_concept gets re-shaped on the next frame by the
        // cursor-tracking concept.read path (memo cleared below). The
        // backing concept data is restored so the drift badge keeps
        // its synced_against until the fresh reply lands.
        self.preview_concept = None;
        self.concept = snap.concept;
        self.concept_target_fired = None;
        self.file_ast_hashes = snap.file_ast_hashes;
        self.file_parse_fired = snap.file_parse_fired;
        // Drop latched fires that never produced a hash: either long-dead
        // in-flights or a failed parse whose retry record was lost on
        // switch-away (`file_parse_retry` is deliberately not snapshotted),
        // which would otherwise restore as an un-re-armable latch — an
        // eternal "checking…" for exactly that path. Re-firing once on
        // restore is cheap and correct.
        self.file_parse_fired
            .retain(|p| self.file_ast_hashes.contains_key(p));
        self.tmux_capture_fired_for = snap.tmux_capture_fired_for;
        // Restore the edit modal — including dirty/discard/stale
        // banners — and re-shape its preview from the buffer.
        self.edit_state = snap.edit_state;
        self.rebuild_edit_preview();
        // Clear the stale Quads then repaint from the cached source.
        // render_preview_source rebuilds preview_md/png/svg for the
        // restored mime; if the leaving workspace had nothing rendered
        // we just leave the panes empty.
        self.preview_png = None;
        self.preview_svg = None;
        if let Some((mime, bytes)) = snap.preview_src.clone() {
            self.preview_src = Some((mime.clone(), bytes.clone()));
            self.render_preview_source(&mime, &bytes);
        } else {
            self.preview_src = None;
        }
        self.window.request_redraw();
        true
    }

    /// Capture the current REPL pane state into the per-workspace
    /// snapshot map. Called from `switch_to_workspace` alongside
    /// `snapshot_current_workspace_ui` so leaving a workspace mid-eval
    /// (or mid-typing) survives the round trip.
    fn snapshot_current_workspace_repl(&mut self) {
        let key = self.current_workspace_key();
        self.workspace_repl_snapshots.insert(
            key,
            WorkspaceReplSnapshot {
                repl_log: self.repl_log.clone(),
                repl_input: self.repl_input.clone(),
                repl_eval_counter: self.repl_eval_counter,
                repl_pkg_mode: self.repl_pkg_mode,
                repl_scroll: self.repl_scroll,
                history_pos: self.history_pos,
                history_saved: self.history_saved.clone(),
            },
        );
    }

    /// Restore REPL state from a per-workspace snapshot. Returns true
    /// if a snapshot was found; otherwise resets to a clean REPL.
    fn restore_workspace_repl(&mut self, key: &str) -> bool {
        if let Some(snap) = self.workspace_repl_snapshots.get(key).cloned() {
            self.repl_log = snap.repl_log;
            self.repl_input = snap.repl_input;
            self.repl_eval_counter = snap.repl_eval_counter;
            self.repl_pkg_mode = snap.repl_pkg_mode;
            self.repl_scroll = snap.repl_scroll;
            self.history_pos = snap.history_pos;
            self.history_saved = snap.history_saved;
            true
        } else {
            // First visit — empty REPL.
            self.repl_log.clear();
            self.repl_input.clear();
            self.repl_eval_counter = 0;
            self.repl_pkg_mode = false;
            self.repl_scroll = 0;
            self.history_pos = None;
            self.history_saved = None;
            false
        }
    }

    /// Single entry point for "switch the chrome's active workspace".
    /// Drives the full snapshot/restore dance from one place so the
    /// Sessions-Enter handler, the workspace-create handler, and the
    /// future cycle-hotkey all behave identically.
    ///
    /// Steps:
    /// 1. Snapshot the *leaving* workspace's UI so a switch-back is
    ///    instant.
    /// 2. Set `active_workspace_id` to the new slug (`None` = default).
    /// 3. Retarget the BL pty to the new workspace's tmux session.
    /// 4. Try to restore from the entering workspace's snapshot —
    ///    if hit, the chrome repaints from cached state and no wire
    ///    request fires.
    /// 5. Otherwise: clear transient view state (so the leaving
    ///    workspace's preview doesn't bleed), fire `tree.root` against
    ///    the new workspace, and refresh `workspace.list` so the
    ///    Sessions row's `kernel_running` badge stays current.
    /// 6. Persist `last_workspace_id` (and the resumed mode/target)
    ///    for the next launch.
    ///
    /// `tmux_session` is `Some(name)` when the caller already has the
    /// target name (Sessions-Enter, workspace.create reply); `None`
    /// derives it from `paths::tmux_session_name(slug)` semantics —
    /// i.e. `sot-be-<slug>`. The default workspace (`slug = None`)
    /// keeps the current BL pane target.
    fn switch_to_workspace(&mut self, slug: Option<String>, tmux_session: Option<String>) {
        self.snapshot_current_workspace_ui();
        self.snapshot_current_workspace_repl();
        self.active_workspace_id = slug.clone();
        if let Some(target) = tmux_session.or_else(|| slug.as_ref().map(|s| format!("sot-be-{s}")))
        {
            self.attach_session_to_bl(target);
        }
        let key = self.current_workspace_key();
        // Restore REPL state independently of the UI snapshot — they
        // travel in parallel and either may be missing (e.g. a first
        // visit to a workspace whose UI is already cached has no REPL
        // snapshot yet).
        let _ = self.restore_workspace_repl(&key);
        let restored = self.restore_workspace_ui(&key);
        if !restored {
            // First visit — start from a clean slate.
            self.mode = Mode::Files;
            self.preview_node_id_fired = None;
            self.pinned_preview_node_id = None;
            self.preview_src = None;
            self.preview_png = None;
            self.preview_svg = None;
            self.preview_concept = None;
            self.concept = None;
            self.concept_target_fired = None;
            self.file_ast_hashes.clear();
            self.file_parse_fired.clear();
            self.tmux_capture_fired_for = None;
            self.edit_state = None;
            self.preview_edit = None;
            if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::TreeRoot {
                mode: "files".to_string(),
                workspace_id: self.active_workspace_id.clone(),
            }) {
                tracing::warn!(error = %e, "drop tree.root after workspace switch");
            }
        }
        // Refresh the workspace list so kernel_running / new rows stay
        // current — cheap and not user-facing if Sessions mode isn't
        // visible. The reply just updates the cached registry view.
        let _ = self
            .req_tx
            .send(crate::transport::OutgoingReq::WorkspaceList);
        // Update the connection status now so the chrome reflects the
        // new workspace immediately. The attach_session_to_bl call above
        // briefly sets a transient "attached BL → …" message; rebuild
        // *after* that so the persistent label wins. The next workspace
        // .list response will refresh it again (kernel_running may flip).
        self.rebuild_connection_status();
        self.persist_resume_state();
        // Badge floor (ADR 0025 §1): if we just switched to a workspace that
        // had a pending `nav.preview` result, drive it now and clear the badge.
        // Resolve the switched-to slug the same way `handle_nav_envelope`'s gate
        // does (active id, falling back to the default workspace's slug) so the
        // key matches what `mark_pending_nav` recorded.
        let switched_slug = self
            .active_workspace_id
            .clone()
            .or_else(|| self.default_workspace_slug.clone());
        if let Some(slug) = switched_slug {
            if let Some(path) = self.pending_nav.remove(&slug) {
                self.mode = Mode::Files;
                let node_id = format!("files:{path}");
                let (fit_w, fit_h) = self.preview_fit_px();
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::PreviewGet {
                    node_id: node_id.clone(),
                    workspace_id: self.active_workspace_id.clone(),
                    page: None,
                    fit_w,
                    fit_h,
                }) {
                    tracing::warn!(error = %e, %node_id,
                        "pending nav.preview: drop preview.get on switch — channel closed");
                } else {
                    self.preview_node_id_fired = Some(node_id.clone());
                    self.preview_anchor_line = None;
                    // #4: land the nav cursor on the driven file so cursor +
                    // preview stay in sync. Two cases, keyed on `restored`:
                    if restored {
                        // Revisit: restore_workspace_ui put the snapshot tree
                        // back and sent NO tree.root (the `!restored` guard
                        // above), so a tree.root-gated reveal would never fire —
                        // the original #4 gap, and exactly the maintainer's case (his was
                        // a revisit). The rows are present now, so reveal
                        // immediately: `drive_reveal_step` lands a visible row or
                        // expands a collapsed ancestor, overriding the stale
                        // restored cursor.
                        // Hold the per-frame preview-follow off the stale cursor
                        // row while a deep (async) reveal lands, so
                        // `maybe_fire_preview` can't clobber the driven badge
                        // preview with the cursor's file (the post-relaunch
                        // badge-consume race). Mirrors `drive_same_ws_open`;
                        // `drive_reveal_step` clears the hold when it lands.
                        if !self.tree.rows.iter().any(|r| r.node.id == node_id) {
                            self.driven_preview_hold_cursor = self
                                .tree
                                .rows
                                .get(self.tree.selected)
                                .map(|r| r.node.id.clone());
                        }
                        self.pending_reveal = Some(node_id.clone());
                        self.drive_reveal_step();
                    } else {
                        // First visit: a tree.root was requested but its rows
                        // aren't in yet, so arm a one-shot reveal consumed on
                        // that reply (see the TreeRoot handler).
                        self.pending_switch_reveal = Some(node_id.clone());
                    }
                    self.status = format!("nav ← agent (pending) · {path}");
                    tracing::info!(%node_id, ws = %slug,
                        "pending nav.preview driven on workspace switch");
                }
            }
        }
        self.window.request_redraw();
    }

    /// Sessions-mode workspace picker entry point (ADR 0014). Opens a
    /// directory-tree browser rooted at `$SOT_PROJECTS_ROOT` (or
    /// `$HOME` if that's unset/missing), kicks off the first
    /// `directory.list` request, and parks the cursor on the first
    /// entry once it arrives. The legacy label-only prompt
    /// (`begin_create_session` + `confirm_create_session`) was
    /// superseded — users browse to an existing directory rather than
    /// typing a path that might not exist.
    fn begin_create_session(&mut self) {
        // Default-root for the picker. Priority:
        //   0. `[sessions] new_session_root` setting — the user's configured
        //      projects root (a BACKEND path); the knob for "start the picker
        //      at my dev dir, not $HOME".
        //   1. $SOT_PROJECTS_ROOT — explicit env override, e.g. someone
        //      wants the picker to start under a specific projects dir.
        //   2. Active host's `remote_home` from hosts.toml — the
        //      right answer for the cross-machine case (the picker
        //      browses the backend's filesystem, not the frontend's; the FRONTEND's
        //      $HOME is meaningless to the BACKEND).
        //   3. The launcher-set SOT_REMOTE_HOME, if it propagated.
        //   4. Frontend's own $HOME — useful only for local hosts.
        //   5. Filesystem root.
        let active_host_home = self
            .selected_host
            .as_deref()
            .or(self.hosts_config.default_host.as_deref())
            .and_then(|name| {
                self.hosts_config
                    .hosts
                    .iter()
                    .find(|h| h.name == name)
                    .and_then(|h| h.remote_home.clone())
            });
        let start = self
            .settings
            .new_session_root
            .clone()
            .or_else(|| std::env::var("SOT_PROJECTS_ROOT").ok())
            .or(active_host_home)
            .or_else(|| std::env::var("SOT_REMOTE_HOME").ok())
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_else(|| "/".to_string());
        self.workspace_picker = Some(WorkspacePicker {
            current_path: start.clone(),
            entries: Vec::new(),
            selected: 0,
        });
        if let Err(e) = self
            .req_tx
            .send(crate::transport::OutgoingReq::DirectoryList {
                path: start.clone(),
            })
        {
            tracing::warn!(error = %e, %start, "drop initial directory.list — channel closed");
        }
        self.status = format!("create workspace · picker @ {start}");
        self.window.request_redraw();
    }

    /// Move the picker's cursor up by one row (saturating).
    fn picker_cursor_up(&mut self) {
        if let Some(p) = self.workspace_picker.as_mut() {
            if p.selected > 0 {
                p.selected -= 1;
            }
            self.window.request_redraw();
        }
    }

    /// Move the picker's cursor down by one row (clamped to entries
    /// length). Zero-entry directories pin the cursor at 0.
    fn picker_cursor_down(&mut self) {
        if let Some(p) = self.workspace_picker.as_mut() {
            if p.selected + 1 < p.entries.len() {
                p.selected += 1;
            }
            self.window.request_redraw();
        }
    }

    /// Drill into the cursored directory: re-fire `directory.list` for
    /// its path and clear the entry list pending the response. Updates
    /// `current_path` immediately so the title reflects where the user
    /// is going even before the listing lands.
    fn picker_drill_in(&mut self) {
        let next = match self.workspace_picker.as_ref() {
            Some(p) => p.entries.get(p.selected).map(|e| e.path.clone()),
            None => None,
        };
        if let Some(path) = next {
            if let Some(p) = self.workspace_picker.as_mut() {
                p.current_path = path.clone();
                p.entries.clear();
                p.selected = 0;
            }
            if let Err(e) = self
                .req_tx
                .send(crate::transport::OutgoingReq::DirectoryList { path: path.clone() })
            {
                tracing::warn!(error = %e, %path, "drop directory.list (drill-in)");
            }
            self.status = format!("picker · {path}");
            self.window.request_redraw();
        }
    }

    /// Ascend to the parent of the picker's `current_path`. Re-fires
    /// the listing so the parent's entries populate.
    fn picker_ascend(&mut self) {
        let parent = match self.workspace_picker.as_ref() {
            Some(p) => std::path::Path::new(&p.current_path)
                .parent()
                .map(|p| p.to_string_lossy().into_owned()),
            None => None,
        };
        if let Some(path) = parent {
            if path.is_empty() {
                return;
            }
            if let Some(p) = self.workspace_picker.as_mut() {
                p.current_path = path.clone();
                p.entries.clear();
                p.selected = 0;
            }
            if let Err(e) = self
                .req_tx
                .send(crate::transport::OutgoingReq::DirectoryList { path: path.clone() })
            {
                tracing::warn!(error = %e, %path, "drop directory.list (ascend)");
            }
            self.status = format!("picker · {path}");
            self.window.request_redraw();
        }
    }

    /// Commit the *cursored sub-directory* as the new workspace's
    /// `project_root`. Falls back to `current_path` if the picker has
    /// no entries (so committing in an empty directory still works).
    /// Label is derived from the basename. Fires `workspace.create`;
    /// the response handler closes the picker and refreshes the
    /// Sessions list.
    fn picker_confirm_selected(&mut self, agent: &str) {
        let path = match self.workspace_picker.as_ref() {
            Some(p) => p
                .entries
                .get(p.selected)
                .map(|e| e.path.clone())
                .unwrap_or_else(|| p.current_path.clone()),
            None => return,
        };
        self.commit_workspace_create(path, agent);
    }

    fn commit_workspace_create(&mut self, path: String, agent: &str) {
        let label = std::path::Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "workspace".to_string());
        if let Err(e) = self
            .req_tx
            .send(crate::transport::OutgoingReq::WorkspaceCreate {
                label: label.clone(),
                project_root: path.clone(),
                autostart_claude: agent == "claude",
                agent: agent.to_string(),
            })
        {
            tracing::warn!(error = %e, %label, %path, "drop workspace.create — channel closed");
            self.status = "create failed · channel closed".to_string();
            return;
        }
        // Status line reflects which Enter the user pressed (ADR 0031):
        // Enter=claude workspace · Shift+Enter=bare · Ctrl+Enter=codex.
        let kind = match agent {
            "claude" => "workspace",
            "codex" => "codex workspace",
            _ => "bare session",
        };
        self.status = format!("create {kind} · '{label}' @ {path} (registering…)");
        self.window.request_redraw();
    }

    /// Cancel the picker without creating anything.
    fn picker_cancel(&mut self) {
        if self.workspace_picker.is_some() {
            self.workspace_picker = None;
            self.status = "create cancelled".to_string();
            self.window.request_redraw();
        }
    }

    /// Sessions-mode (B4): finalize the typed label. Derives the tmux
    /// session name + project directory via the same slug rule the
    /// backend uses (paths::slug) so a `--label foo` daemon and the
    /// frontend agree on the name. `cwd = $SOT_PROJECTS_ROOT/<label>`
    /// (defaults to `$HOME/julia_dev/<label>` on Linux).
    /// Sessions-mode (ADR 0013): if the selected row is a session or a
    /// pane, return the session name (panes have a `session` payload).
    /// `None` for any other row kind.
    fn selected_session_name(&self) -> Option<String> {
        let row = self.tree.rows.get(self.tree.selected)?;
        if row.node.kind == "session" {
            return row
                .node
                .payload
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
        if row.node.kind == "pane" {
            return row
                .node
                .payload
                .get("session")
                .and_then(|v| v.as_str())
                .map(String::from);
        }
        None
    }

    /// True while a recent auto-start launch into `session` is still inside
    /// its boot grace window (`AUTOSTART_LAUNCH_HOLD`). The attach guard uses
    /// this to suppress a *second* launch during ccb startup (the
    /// no-double-launch half); once the stamp ages past the window the guard
    /// stops skipping, so a launch lost to a rapid pty re-target retries on
    /// the next attach (the no-permanent-block half). A confirmed launch is
    /// promoted out of `launching_sessions` by the 4171 sniff before it
    /// expires, so this only ever returns true for genuinely in-flight or
    /// lost-but-still-recent launches.
    fn launching_held(&self, session: &str) -> bool {
        self.launching_sessions
            .get(session)
            .is_some_and(|t| t.elapsed() < AUTOSTART_LAUNCH_HOLD)
    }

    /// Sessions-mode (ADR 0013) attach action (B3): re-target the BL pane
    /// to the selected session. For a `session` row, attach to that
    /// session's name; for a `pane` row, attach to the parent session
    /// (selecting a specific pane within the session is left to tmux's
    /// own default, which restores the most-recently-active pane).
    /// Sends a `pty.open` carrying the new target; backend kills the
    /// existing pty and respawns against the new session.
    fn attach_session_to_bl(&mut self, session_name: String) {
        if self.bl_pane_target.as_deref() == Some(session_name.as_str()) {
            // Already attached — no-op rather than churn the pty.
            self.status = format!("already attached · {session_name}");
            self.window.request_redraw();
            return;
        }
        let (cols, rows) = self.pty_size.unwrap_or((80, 24));
        if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::PtyOpen {
            cols,
            rows,
            target: Some(session_name.clone()),
            // #5 guard: attach_session_to_bl is reached only via an explicit
            // user workspace-switch (switch_to_workspace / Sessions-mode
            // attach), so this open IS allowed to re-target the foreground pty.
            user_switch: true,
        }) {
            tracing::warn!(error = %e, %session_name, "drop pty.open re-target request — channel closed");
            return;
        }
        self.bl_pane_target = Some(session_name.clone());
        self.status = format!("attached BL → {session_name}");
        // Claude boot is owned by the BE tmux start-command wrapper
        // (`boot_wrapper_command`, ADR 0023 unified spawn): every
        // `autostart_claude` workspace is created with the wait-for-attach
        // wrapper as its pane command, so claude `exec`s the instant THIS
        // attach flips `session_attached>0` — no FE-typed `ccb`, no prompt
        // race. The old FE autostart-on-attach launch (pending_autostart →
        // advance_autostart_scan → autostart_claude_in_pane) is retired; the
        // single boot path is the wrapper.
        self.window.request_redraw();
        self.persist_resume_state();
    }

    /// Decide an armed pre-launch sniff (contract b). Once tmux's replayed
    /// screen has settled, scan it: if claude is already running in the pane,
    /// skip the launch (typing `claude …` would land as a prompt in the live
    /// agent — the spam this guards, esp. across FE relaunches that forget
    /// `autostarted_sessions`); otherwise fire the real launch. Re-checks the
    /// pinned target so a user switch-away cancels cleanly.
    fn advance_autostart_scan(&mut self) {
        // Wait for the replayed screen before scanning, but cap so a silent
        // pane (no output) still launches.
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(1200);
        const MIN_WAIT: std::time::Duration = std::time::Duration::from_millis(600);
        const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

        let (session, started, last_pty) = match self.autostart_scan.as_ref() {
            Some(s) => (s.session.clone(), s.started, s.last_pty),
            None => return,
        };
        // User switched the BL pane away before we decided — DEFER, don't
        // kill: `autostarted_sessions` stays untouched so the next attach of
        // this session re-arms the launch (attach_session_to_bl's contract-b
        // check), and the status says so out loud — a silent flash here made
        // spawn delivery look like a coin flip under a multitasking user.
        if self.bl_pane_target.as_deref() != Some(session.as_str()) {
            tracing::warn!(%session,
                "autostart: BL pane left the target before launch decision — deferred to next attach");
            self.status = format!("autostart deferred · launches on next visit → {session}");
            self.autostart_scan = None;
            self.window.request_redraw();
            return;
        }
        let now = std::time::Instant::now();
        let settled =
            now.duration_since(last_pty) >= SETTLE && now.duration_since(started) >= MIN_WAIT;
        let timed_out = now.duration_since(started) >= MAX_WAIT;
        if !settled && !timed_out {
            return; // still replaying — check again next tick
        }

        // Only trust a positive "already running" if the pane produced
        // output AFTER this attach armed the scan: until the re-target's
        // replay lands, `contents()` is still the PREVIOUS session's screen,
        // and matching the user's own claude there poisons
        // `autostarted_sessions` for the agent session — a silently dead
        // spawn. (A fresh attach always replays at least a prompt, so an
        // honest match implies fresh output.)
        let output_since_attach = last_pty > started;
        let contents = self.pty_terminal.screen().contents();
        if pane_shows_running_claude(&contents) {
            if output_since_attach {
                // Already running — record it as CONFIRMED so we treat the
                // pane as satisfied and never type the launch string into the
                // live agent's prompt. Promote: clear any "launching" hold now
                // that the launch is verified up.
                self.autostarted_sessions.insert(session.clone());
                self.launching_sessions.remove(&session);
                self.autostart_scan = None;
                tracing::info!(%session,
                    "autostart: claude already running in pane — skipping launch (no prompt-spam)");
                self.status =
                    format!("auto-start: claude already running in {session} — left as-is");
                self.window.request_redraw();
                return;
            }
            tracing::info!(%session,
                "autostart: 'running claude' match predates any post-attach output — stale screen, ignoring");
        }
        // No (trustworthy) claude detected — fire the real launch + task
        // delivery. PtyOpened acked the re-target, so the write lands in
        // this session's pane.
        self.autostart_scan = None;
        self.autostart_claude_in_pane(session);
    }

    /// Contract (b): launch claude in the freshly-attached agent pane, then
    /// hand task delivery to the event-loop state machine (`advance_delivery`).
    /// The keys flow through the FE-attached pty (`pty.write` → BL pane): that
    /// live terminal client is exactly what a detached session lacks, and why
    /// claude can't self-init there.
    fn autostart_claude_in_pane(&mut self, session: String) {
        let (task, agent) = match self.workspace_autostart.get(&session) {
            Some(info) => (info.task.clone(), info.agent_name.clone()),
            None => return,
        };
        // Only if the BL pane is actually on this session right now.
        if self.bl_pane_target.as_deref() != Some(session.as_str()) {
            return;
        }
        // Stamp a time-bounded "launching" hold (NOT the permanent confirmed
        // mark — that's set only by the 4171 sniff once ccb is actually seen
        // running). This blocks a rapid re-attach from double-firing during
        // boot, but ages out (AUTOSTART_LAUNCH_HOLD) so a launch lost to a pty
        // re-target retries on the next attach instead of being stuck forever.
        self.launching_sessions
            .insert(session.clone(), std::time::Instant::now());
        // One launch flavor (pane is already cd'd to the repo root): `ccb` —
        // claude whose first turn is the /sot-session-start receive-bootstrap,
        // so every agent session comes up comm-aware (joined + listening +
        // inbox Monitor armed) with no hand-rolled join in the brief (maintainer decision,
        // 2026-06-12; comm-spawn's brief is now task-only). A spawned agent's
        // handle is pinned with SOT_COMM_NAME=<agent> — comm-join's env
        // default — so the spawner knows who reports back. Tilde path because
        // the daemon-made tmux login shell may not have ~/.local/bin on PATH
        // (same pitfall comm-spawn.sh works around); bash expands the tilde,
        // and a missing ccb fails loudly in the pane rather than silently.
        // The task brief (if any) is delivered by `advance_delivery` after
        // the bootstrap turn settles — its TIMEOUT is sized to ride that
        // turn out.
        let launch = if agent.is_empty() {
            "~/.local/bin/ccb\r".to_string()
        } else {
            format!("SOT_COMM_NAME={agent} ~/.local/bin/ccb\r")
        };
        self.status = if !task.is_empty() {
            format!("auto-starting ccb · @{agent}")
        } else {
            format!("auto-starting ccb · {session}")
        };
        tracing::info!(%session, %agent, has_task = !task.is_empty(),
            "autostart: launching ccb in agent pane");
        if self
            .req_tx
            .send(crate::transport::OutgoingReq::PtyWrite {
                bytes: launch.into_bytes(),
            })
            .is_err()
        {
            return;
        }
        if task.is_empty() {
            return; // launch-only; nothing to deliver
        }
        let now = std::time::Instant::now();
        self.delivery = Some(AutoStartDelivery {
            pinned: session,
            task,
            agent,
            phase: DeliveryPhase::Boot,
            last_pty: now,
            started: now,
            submitted_at: None,
        });
        self.window.request_redraw();
    }

    /// Drive an in-flight auto-start task delivery (contract b). Called from
    /// the event loop each tick/frame. Waits for the pinned pane's pty output
    /// to settle (readiness — not a blind timer), clears a possible what's-new
    /// interstitial, types the bootstrap, then settles + double-Enter-submits.
    /// Re-checks `bl_pane_target == pinned` before every keystroke so it never
    /// misroutes into a session the user switched to, and on overall timeout
    /// surfaces a status notice instead of leaving a launched-but-idle agent.
    fn advance_delivery(&mut self) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);
        const MIN_BOOT: std::time::Duration = std::time::Duration::from_secs(3);
        const SUBMIT_GAP: std::time::Duration = std::time::Duration::from_millis(800);
        // Sized to ride out the agent's /sot-session-start first turn: the
        // ccb launch runs the full comm bootstrap (join + listener + Monitor
        // + selftest + poll) before the pane settles enough for the brief,
        // and that turn alone can run well past the old 60s.
        const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

        let (pinned, phase, last_pty, started, submitted_at, agent) = match self.delivery.as_ref() {
            Some(d) => (
                d.pinned.clone(),
                d.phase,
                d.last_pty,
                d.started,
                d.submitted_at,
                d.agent.clone(),
            ),
            None => return,
        };
        let now = std::time::Instant::now();

        // Pin: never write to a pane the user switched to. DEFER, don't
        // kill (mirror of e69cd45's launch defer): the agent's tmux pane
        // lives on regardless of where the BL pane looks, so the delivery
        // resumes phase-preserved on the next attach of the pinned session
        // (PtyOpened consumes `deferred_delivery`). Killing here left a
        // launched-but-taskless agent behind a status flash.
        if self.bl_pane_target.as_deref() != Some(pinned.as_str()) {
            tracing::warn!(%pinned,
                "autostart: BL pane left the target before delivery — deferred to next attach (no misroute)");
            self.status = format!(
                "auto-start: task for @{agent} deferred · delivers on next visit → {pinned}"
            );
            self.deferred_delivery = self.delivery.take();
            self.window.request_redraw();
            return;
        }
        // Timeout: surface rather than leave a launched-but-idle agent.
        if now.duration_since(started) > TIMEOUT {
            tracing::warn!(%pinned,
                "autostart: claude prompt not confirmed within timeout — task not delivered");
            self.status = format!(
                "auto-start: couldn't confirm @{agent}'s prompt in 60s — task NOT delivered, do it manually"
            );
            self.delivery = None;
            self.window.request_redraw();
            return;
        }

        let settled = now.duration_since(last_pty) >= SETTLE;
        match phase {
            DeliveryPhase::Boot => {
                // Wait for the TUI to come up and quiesce, then clear a
                // possible what's-new interstitial (no-op on an empty prompt).
                if now.duration_since(started) >= MIN_BOOT && settled {
                    self.deliver_write(b"\r".to_vec());
                    if let Some(d) = self.delivery.as_mut() {
                        d.phase = DeliveryPhase::Interstitial;
                        d.last_pty = now;
                    }
                }
            }
            DeliveryPhase::Interstitial => {
                // Prompt is up (interstitial cleared); type the bootstrap.
                if settled {
                    let task = self
                        .delivery
                        .as_ref()
                        .map(|d| d.task.clone())
                        .unwrap_or_default();
                    self.deliver_write(task.into_bytes());
                    if let Some(d) = self.delivery.as_mut() {
                        d.phase = DeliveryPhase::Typed;
                        d.last_pty = now;
                    }
                }
            }
            DeliveryPhase::Typed => {
                // Task text settled in the input box; submit (Enter #1).
                if settled {
                    self.deliver_write(b"\r".to_vec());
                    if let Some(d) = self.delivery.as_mut() {
                        d.phase = DeliveryPhase::Submitted;
                        d.submitted_at = Some(now);
                    }
                }
            }
            DeliveryPhase::Submitted => {
                // Ink can drop the first Enter after a big paste — confirm with
                // a second one, then we're done.
                let gap_ok = submitted_at
                    .map(|t| now.duration_since(t) >= SUBMIT_GAP)
                    .unwrap_or(true);
                if gap_ok {
                    self.deliver_write(b"\r".to_vec());
                    self.status = format!("auto-start: delivered task to @{agent}");
                    self.delivery = None;
                    self.window.request_redraw();
                }
            }
        }
    }

    /// Send one keystroke to the BL pane during delivery. The caller has
    /// already confirmed `bl_pane_target == pinned`.
    fn deliver_write(&self, bytes: Vec<u8>) {
        let _ = self
            .req_tx
            .send(crate::transport::OutgoingReq::PtyWrite { bytes });
    }

    /// Sessions-mode (ADR 0013): when the cursored row is a `pane`, fire a
    /// fresh `tmux.capture_pane` so the preview pane shows its live tail.
    /// Dedupes per pane (`tmux_pane_id` payload field) so cursor hovers
    /// don't spam requests. Called from `redraw` alongside the other
    /// cursor-driven maybe_fire_* hooks.
    fn maybe_fire_tmux_capture(&mut self) {
        if !matches!(self.mode, Mode::Sessions) {
            return;
        }
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        if row.node.kind != "pane" {
            return;
        }
        let Some(pane_id) = row
            .node
            .payload
            .get("tmux_pane_id")
            .and_then(|v| v.as_str())
        else {
            return;
        };
        if self.tmux_capture_fired_for.as_deref() == Some(pane_id) {
            return;
        }
        let target = pane_id.to_string();
        if let Err(e) = self
            .req_tx
            .send(crate::transport::OutgoingReq::TmuxCapturePane {
                target: target.clone(),
                lines: 200,
            })
        {
            tracing::warn!(error = %e, %target, "drop tmux.capture_pane request — channel closed");
            return;
        }
        self.tmux_capture_fired_for = Some(target);
    }

    /// If the selected tree row's annotation target differs from the last
    /// one we asked the backend about, fire a fresh `concept.read`. Called
    /// from `redraw` so cursor moves and event-driven tree updates both
    /// trigger refresh without each caller having to remember.
    fn maybe_fire_concept_read(&mut self) {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return;
        };
        let target = node_id_to_concept_target(&row.node.id);
        // Only fire file.parse for real Julia source. Directories get an
        // io_error from the kernel; binary files (.h5, .png, .arrow, …)
        // make the JuliaSyntax parser walk huge byte streams looking for
        // valid syntax and block the kernel queue for every subsequent
        // request — observed as a full-app freeze when the cursor lands
        // on a multi-MB HDF5 file. Restrict to `.jl` so the drift check
        // only runs where it can possibly succeed.
        let files_path = if row.node.kind == "dir" {
            None
        } else {
            row.node.id.strip_prefix("files:").and_then(|p| {
                if p.is_empty() || !p.ends_with(".jl") {
                    None
                } else {
                    Some(p.to_string())
                }
            })
        };
        let node_label = row.node.label.clone();
        if self.concept_target_fired == target {
            // Cursor didn't move; but the cursored row might still need a
            // file.parse fired (first time we see it). Fall through to the
            // file-parse check below without re-firing concept.read.
        } else {
            if let Some(t) = target.as_ref() {
                if let Err(e) = self
                    .req_tx
                    .send(crate::transport::OutgoingReq::ConceptRead {
                        target: t.clone(),
                        workspace_id: self.active_workspace_id.clone(),
                    })
                {
                    tracing::warn!(error = %e, target = %t, "drop concept.read request — channel closed");
                    return;
                }
            }
            self.concept_target_fired = target;
            // Clear stale cached result so the chrome doesn't keep showing
            // the previous node's annotation status while the new read is
            // in flight.
            self.concept = None;
            self.preview_concept = None;
        }
        // Drift check: ask the kernel for the file's ast_hash once per
        // distinct path the user has visited. The HashMap grows over the
        // session; phase-2 may add a TTL or eager sweep.
        if let Some(path) = files_path {
            // Retry gate for failed parses: re-arm the one-shot latch only
            // after an exponential backoff (`1u64 << n` with n=1,2 → 2s,
            // 4s), and stop entirely at the attempt cap — at the cap the
            // latch STAYS latched, so a storm is structurally impossible:
            // each re-arm buys exactly one fire (the latch re-inserts on
            // fire; the failure handler re-stamps the timestamp).
            if !self.file_ast_hashes.contains_key(&path) {
                if let Some(&(failed_at, attempts)) = self.file_parse_retry.get(&path) {
                    if attempts < FILE_PARSE_MAX_RETRIES
                        && failed_at.elapsed()
                            >= std::time::Duration::from_secs(1u64 << attempts.min(4))
                    {
                        self.file_parse_fired.remove(&path);
                    }
                }
            }
            if !self.file_ast_hashes.contains_key(&path)
                && self.file_parse_fired.insert(path.clone())
            {
                tracing::debug!(%path, label = %node_label, "→ file.parse for drift check");
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::FileParse {
                    path: path.clone(),
                    workspace_id: self.active_workspace_id.clone(),
                }) {
                    tracing::warn!(error = %e, %path, "drop file.parse request — channel closed");
                    self.file_parse_fired.remove(&path);
                }
            }
        }
    }

    /// Send the current REPL input buffer to the backend as a `repl.eval`
    /// request, push an in-flight entry into the scrollback, and clear the
    /// input. Empty input is a no-op (no point round-tripping whitespace).
    /// The eval_id is locally generated; the response handler reconciles
    /// by matching on it.
    /// Branch on mime and route a preview blob to the right renderer.
    /// Called both from the live `Preview` event arm and from the
    /// font-rescale path (which replays against the cached source).
    /// Translate `math_cache` into the per-block pixel-metrics map the
    /// markdown walk consumes. Each cached SVG's ex-unit dimensions
    /// become unscaled pixels in body-font space:
    /// `value_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE`. The walk applies
    /// the per-context `scale` itself, so the values stored here are
    /// scale-1 reference numbers — same coordinate system as
    /// `MATH_BLOCK_H_DEFAULT` and friends. Entries without parsed
    /// dimensions are skipped (the walk will see no cache hit and use
    /// its fallback path).
    fn build_math_metrics(&self) -> MathMetricsMap {
        let mut out = MathMetricsMap::new();
        for (key, entry) in self.math_cache.iter() {
            let (Some(w_ex), Some(h_ex)) = (entry.width_ex, entry.height_ex) else {
                continue;
            };
            let width_px = w_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE;
            let height_px = h_ex * MATHJAX_EX_FACTOR * MD_BODY_SIZE;
            // vertical_align_ex is negative when the SVG hangs below
            // the baseline. Drop is the positive distance below; default
            // 0 for display blocks that don't always include the style.
            let baseline_drop_px = entry
                .vertical_align_ex
                .map(|v| (-v) * MATHJAX_EX_FACTOR * MD_BODY_SIZE)
                .unwrap_or(0.0);
            out.insert(
                key.clone(),
                MathMetrics {
                    width_px,
                    height_px,
                    baseline_drop_px,
                },
            );
        }
        out
    }

    /// Translate `figure_cache` into the per-figure pixel metrics map
    /// the markdown walk consumes. Walk uses these to size each
    /// `![](url)` placeholder's reserved line height so the layout
    /// doesn't reshape when the figure finishes loading. Natural
    /// dimensions are scale-1 pixels — the walk applies `Ctx::scale`
    /// itself.
    fn build_figure_metrics(&self) -> FigureMetricsMap {
        let mut out = FigureMetricsMap::new();
        for (url, entry) in self.figure_cache.iter() {
            out.insert(
                url.clone(),
                FigureMetrics {
                    width_px: entry.natural_w_px as f32,
                    height_px: entry.natural_h_px as f32,
                },
            );
        }
        // Terminal failures report as 0-size: the markdown walk reads
        // height_px <= 0 as "will never paint" and collapses the
        // reservation to its compact text fallback.
        for url in self.figure_failed.iter() {
            out.entry(url.clone()).or_insert(FigureMetrics {
                width_px: 0.0,
                height_px: 0.0,
            });
        }
        out
    }

    /// Fire `figure.get` for every `MediaBlock::Figure` in the latest
    /// markdown preview that isn't already cached or in flight.
    /// Resolves the literal URL against the current markdown file's
    /// directory (`preview_node_id_fired`) so a `![](sample.png)` in
    /// `examples/preview/foo.md` lands as
    /// `files:examples/preview/sample.png`. URLs we can't resolve
    /// (remote, absolute, `..`-walking, no current md file) are
    /// silently skipped — better than a request the backend will
    /// reject.
    fn dispatch_pending_figures(&mut self) {
        let blocks = self.preview_md.media_blocks.clone();
        let md_node_id = self.current_md_node_id.clone();
        let workspace_id = self.current_md_workspace_id.clone();
        for block in blocks {
            let crate::preview::markdown::MediaBlock::Figure { url, .. } = block else {
                continue;
            };
            if self.figure_cache.contains_key(&url)
                || self.figure_pending.contains(&url)
                || self.figure_failed.contains(&url)
            {
                continue;
            }
            let Some(node_id) = resolve_figure_node_id(&md_node_id, &url) else {
                // Local-but-unresolvable (walks out of root, no current md
                // file): terminal — mark failed so the layout collapses its
                // reservation instead of holding an empty box forever.
                // (Remote URLs never get this far: the walk renders them as
                // the compact fallback and pushes no MediaBlock.)
                tracing::debug!(%url, "figure unresolvable — collapsing to compact fallback");
                self.figure_failed.insert(url);
                self.needs_md_reflow = true;
                continue;
            };
            if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::FigureGet {
                url: url.clone(),
                node_id,
                workspace_id: workspace_id.clone(),
            }) {
                tracing::warn!(error = %e, %url, "drop figure.get request — channel closed");
                continue;
            }
            self.figure_pending.insert(url);
        }
    }

    /// Persist the current PNG view-state (zoom + centre) under its
    /// `(parent_dir, dims)` key so a sibling render of the same size
    /// can restore it on the next preview load.
    /// Convert a physical-pixel cursor position into LLM-pane cell coords
    /// `(row, col)`. `strict` rejects positions outside the pane rect
    /// (used for mouse-down — don't start a selection on click outside);
    /// when false, the position is clamped to the pane (used for drag —
    /// allow extending past the pane edge). Returns `None` when the LLM
    /// pane has no area (zero rows or cols, e.g. layout collapsed).
    fn llm_cell_at_px(&self, px: (f32, f32), strict: bool) -> Option<(u16, u16)> {
        let rect = self.pane_rects.llm;
        if rect.width == 0 || rect.height == 0 {
            return None;
        }
        let cell_w = self.cell_w.max(1.0);
        let cell_h = self.cell_h.max(1.0);
        let origin_x = self.chrome_origin_x + rect.x as f32 * cell_w;
        let origin_y = self.chrome_origin_y + rect.y as f32 * cell_h;
        let dx = px.0 - origin_x;
        let dy = px.1 - origin_y;
        let pane_w_px = rect.width as f32 * cell_w;
        let pane_h_px = rect.height as f32 * cell_h;
        if strict && (dx < 0.0 || dy < 0.0 || dx >= pane_w_px || dy >= pane_h_px) {
            return None;
        }
        let col = (dx / cell_w).floor().clamp(0.0, rect.width as f32 - 1.0) as u16;
        let row = (dy / cell_h).floor().clamp(0.0, rect.height as f32 - 1.0) as u16;
        Some((row, col))
    }

    /// Walk the LLM-pane selection range in the vt100 grid, build a UTF-8
    /// string, and push it to the OS clipboard via `arboard`. Linear range
    /// (terminal-style line wrap, not rectangular); trailing whitespace on
    /// each line trims. Clears `llm_selection` on success. Returns `true`
    /// iff something was written.
    fn copy_llm_selection(&mut self) -> bool {
        let Some(sel) = self.llm_selection else {
            return false;
        };
        let (a, b) = sel;
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        let (sr, sc) = start;
        let (er, ec) = end;
        let pane_cols = self.pane_rects.llm.width;
        if pane_cols == 0 {
            return false;
        }
        let screen = self.pty_terminal.screen();
        let mut out = String::new();
        for row in sr..=er {
            if row != sr {
                out.push('\n');
            }
            let cs = if row == sr { sc } else { 0 };
            let ce = if row == er {
                ec
            } else {
                pane_cols.saturating_sub(1)
            };
            let mut line = String::new();
            for col in cs..=ce {
                match screen.cell(row, col) {
                    Some(cell) => {
                        let g = cell.contents();
                        if g.is_empty() {
                            line.push(' ');
                        } else {
                            line.push_str(g);
                        }
                    }
                    None => line.push(' '),
                }
            }
            while line.ends_with(' ') {
                line.pop();
            }
            out.push_str(&line);
        }
        if out.is_empty() {
            return false;
        }
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(out.clone())) {
            Ok(()) => {
                tracing::info!(bytes = out.len(), "llm.copy → clipboard");
                self.llm_selection = None;
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "clipboard write failed; selection kept");
                false
            }
        }
    }

    /// Project root for the active workspace, falling back to the
    /// daemon-startup root (from the hello response) if no
    /// `workspace.list` reply has populated `workspace_project_roots`
    /// yet. The active workspace's root — *not* the daemon startup
    /// root — is the right base for joining `files:<rel>` ids on the
    /// backend, because a workspace swap changes the file tree's
    /// meaning of `<rel>` without changing the daemon startup root.
    fn active_project_root(&self) -> Option<&str> {
        if let Some(slug) = self.active_workspace_id.as_deref() {
            if let Some(root) = self.workspace_project_roots.get(slug) {
                return Some(root.as_str());
            }
        }
        self.daemon_project_root.as_deref()
    }

    /// Resolve the cursored NavTree row to a backend-absolute path,
    /// when the row is a `files:<rel>` node and we know the active
    /// workspace's project_root. Used by `o` (open-in-external-tool)
    /// for Pluto-flavored `.jl` dispatch — the backend needs an
    /// absolute path to hand to `SessionActions.open`.
    fn cursored_files_path(&self) -> Option<String> {
        let row = self.tree.rows.get(self.tree.selected)?;
        let rel = row.node.id.strip_prefix("files:")?;
        if rel.is_empty() {
            return None;
        }
        let root = self.active_project_root()?;
        let trimmed = root.trim_end_matches(['/', '\\']);
        Some(format!("{trimmed}/{rel}"))
    }

    /// Preview-pane analogue of `cursored_files_path`: the path of the file
    /// whose preview is currently SHOWING (pinned wins over last-fired).
    /// This can differ from the nav cursor — pinned previews, badge-consumed
    /// previews — so open-style keys pressed with preview focus act on what
    /// the user is LOOKING AT. Callers fall back to the cursored row when
    /// the shown preview isn't a files-mode node.
    fn previewed_files_path(&self) -> Option<String> {
        let id = self
            .pinned_preview_node_id
            .as_deref()
            .or(self.preview_node_id_fired.as_deref())?;
        let rel = id.strip_prefix("files:")?;
        if rel.is_empty() {
            return None;
        }
        let root = self.active_project_root()?;
        let trimmed = root.trim_end_matches(['/', '\\']);
        Some(format!("{trimmed}/{rel}"))
    }

    /// `o` — open `abs` in the right external tool: an html preview body →
    /// temp file + OS browser; `.jl` → backend `pluto.open` (header-checked
    /// there); video → backend `video.open` (browser HTML5 playback);
    /// `.qmd` → quick Quarto render (no execution). Shared by the NavTree
    /// and Preview key arms (same behavior on the cursored / shown file).
    fn open_path_external(&mut self, abs: Option<String>) {
        let preview_mime = self.preview_src.as_ref().map(|(m, _)| m.clone());
        if preview_mime.as_deref() == Some("text/html") {
            if let Some((_, bytes)) = self.preview_src.as_ref() {
                if let Err(e) = open_html_in_browser(bytes) {
                    tracing::warn!(error = %e, "failed to open preview in browser");
                }
            }
        } else if let Some(abs) = abs.as_deref() {
            let lower = abs.to_ascii_lowercase();
            let is_video = [".mp4", ".webm", ".mov", ".mkv", ".m4v"]
                .iter()
                .any(|ext| lower.ends_with(ext));
            if abs.ends_with(".jl") {
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::PlutoOpen {
                    path: abs.to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to dispatch pluto.open");
                }
            } else if is_video {
                // Video plays in the browser (HTML5 <video>, native HW
                // decode) — the pane only shows the poster still.
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::VideoOpen {
                    path: abs.to_string(),
                }) {
                    tracing::warn!(error = %e, "failed to dispatch video.open");
                }
            } else if lower.ends_with(".qmd") {
                // Quarto: `o` = quick render (no code execution) →
                // self-contained HTML in the browser. `O` runs chunks.
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::QuartoOpen {
                    path: abs.to_string(),
                    execute: false,
                }) {
                    tracing::warn!(error = %e, "failed to dispatch quarto.open");
                } else {
                    self.status = "quarto · rendering (quick)…".to_string();
                }
            } else {
                tracing::debug!(path = %abs, "`o` ignored — no handler for this file type");
            }
        }
    }

    /// `W` — open the project's built Documenter site in the OS browser
    /// (ADR 0024), deep-linking `path` when it's a built docs page.
    fn docs_open_external(&mut self, path: String) {
        if let Err(e) = self
            .req_tx
            .send(crate::transport::OutgoingReq::DocsOpen { path })
        {
            tracing::warn!(error = %e, "failed to dispatch docs.open");
        } else {
            self.status = "docs · opening…".to_string();
        }
    }

    /// `O` — full Quarto render WITH code execution for a `.qmd`.
    fn quarto_open_execute(&mut self, abs: Option<String>) {
        if let Some(abs) = abs.as_deref() {
            if abs.to_ascii_lowercase().ends_with(".qmd") {
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::QuartoOpen {
                    path: abs.to_string(),
                    execute: true,
                }) {
                    tracing::warn!(error = %e, "failed to dispatch quarto.open (execute)");
                } else {
                    self.status = "quarto · rendering (run chunks)…".to_string();
                }
            }
        }
    }

    /// `d` in NavTree: download the cursored file row to the local downloads
    /// dir (OS-independent — `Settings::download_dir()`), non-clobbering. The
    /// transport streams chunks and writes the dest as they arrive. Directory
    /// rows are a no-op (download a file, not a folder).
    fn start_download(&mut self) {
        let is_dir = self
            .tree
            .rows
            .get(self.tree.selected)
            .map(|r| r.node.kind == "dir")
            .unwrap_or(false);
        if is_dir {
            self.status = "download · select a file, not a folder".to_string();
            self.window.request_redraw();
            return;
        }
        let Some(abs) = self.cursored_files_path() else {
            self.status = "download · no file under cursor".to_string();
            self.window.request_redraw();
            return;
        };
        let basename = abs
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("download.bin")
            .to_string();
        let dir = self.settings.download_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            tracing::warn!(error = %e, dir = %dir.display(), "download: cannot create downloads dir");
            self.status = format!("download failed · mkdir {}: {e}", dir.display());
            self.window.request_redraw();
            return;
        }
        let dest = crate::download::non_clobbering_path(&dir, &basename);
        if let Err(e) = self
            .req_tx
            .send(crate::transport::OutgoingReq::FileDownload {
                path: abs,
                dest: dest.clone(),
            })
        {
            tracing::warn!(error = %e, "drop file.download — channel closed");
            return;
        }
        self.status = format!("download · {basename} → {}", dest.display());
        self.window.request_redraw();
    }

    /// `u` in NavTree: pick a local file via the native OS dialog and upload it
    /// to the cursored nav folder (the dir itself for a dir row, else the
    /// cursored file's parent dir). Opens the file, then sends the first chunk;
    /// subsequent chunks are pumped by `FileUploadAck` (flow control).
    fn start_upload(&mut self) {
        if self.upload.is_some() {
            self.status = "upload · already in progress".to_string();
            self.window.request_redraw();
            return;
        }
        let (is_dir, node_id) = match self.tree.rows.get(self.tree.selected) {
            Some(r) => (r.node.kind == "dir", r.node.id.clone()),
            None => (false, String::new()),
        };
        let Some(abs) = self.cursored_files_path() else {
            self.status = "upload · no target folder for this row".to_string();
            self.window.request_redraw();
            return;
        };
        // Destination dir + its tree node id: a dir row is the target itself,
        // a file row targets its parent dir. `dir_node_id` lets us refresh the
        // nav listing when the upload completes so the new file appears.
        let (dir, dir_node_id) = if is_dir {
            (abs, node_id)
        } else {
            let dir = match abs.rsplit_once(['/', '\\']) {
                Some((parent, _)) if !parent.is_empty() => parent.to_string(),
                _ => {
                    self.status = "upload · cannot resolve parent folder".to_string();
                    self.window.request_redraw();
                    return;
                }
            };
            (dir, parent_files_node_id(&node_id))
        };
        // Native OS picker (rfd): Win common dialog / macOS NSOpenPanel / Linux
        // GTK-or-XDG-portal. Blocking — the app waits on the modal dialog.
        let picked = rfd::FileDialog::new()
            .set_title("Upload a file to the cursored folder")
            .pick_file();
        let Some(local) = picked else {
            self.status = "upload · cancelled".to_string();
            self.window.request_redraw();
            return;
        };
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "upload.bin".to_string());
        let opened = std::fs::File::open(&local).and_then(|f| {
            let total = f.metadata()?.len();
            Ok((f, total))
        });
        let (file, total) = match opened {
            Ok(ft) => ft,
            Err(e) => {
                tracing::warn!(error = %e, local = %local.display(), "upload: open local file failed");
                self.status = format!("upload failed · open {}: {e}", local.display());
                self.window.request_redraw();
                return;
            }
        };
        self.upload = Some(UploadState {
            file,
            dir,
            dir_node_id,
            name,
            total,
            sent: 0,
        });
        self.send_next_upload_chunk();
    }

    /// Read the next `UPLOAD_CHUNK` from the in-flight upload's local file and
    /// send it as a `file.upload` chunk. Called once to kick off the upload and
    /// again on each non-`done` ack. A read error or closed channel aborts.
    fn send_next_upload_chunk(&mut self) {
        use std::io::Read;
        let read = {
            let Some(up) = self.upload.as_mut() else {
                return;
            };
            let mut buf = vec![0u8; UPLOAD_CHUNK];
            match up.file.read(&mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    let offset = up.sent;
                    up.sent += n as u64;
                    let eof = up.sent >= up.total;
                    Ok((
                        up.dir.clone(),
                        up.name.clone(),
                        up.total,
                        up.sent,
                        offset,
                        eof,
                        buf,
                    ))
                }
                Err(e) => Err(format!("{e}")),
            }
        };
        match read {
            Ok((dir, name, total, sent, offset, eof, bytes)) => {
                if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::FileUpload {
                    dir,
                    name: name.clone(),
                    offset,
                    total,
                    eof,
                    bytes,
                }) {
                    tracing::warn!(error = %e, "drop file.upload chunk — channel closed");
                    self.upload = None;
                } else {
                    self.status = format!("upload · {name} {sent}/{total}");
                }
            }
            Err(e) => {
                let name = self
                    .upload
                    .as_ref()
                    .map(|u| u.name.clone())
                    .unwrap_or_default();
                tracing::warn!(error = %e, "upload: local read failed");
                self.status = format!("upload failed · read {name}: {e}");
                self.upload = None;
            }
        }
        self.window.request_redraw();
    }

    /// Push the cursored NavTree row's file path to the OS clipboard. Only
    /// fires for rows whose node id starts with `files:` (Files mode + the
    /// scan-derived rows in Modules mode that route through preview.get).
    /// Joins with `daemon_project_root` when known to yield an absolute
    /// backend-side path; falls back to the workspace-relative path if
    /// the hello response didn't carry a project_root. Returns true iff
    /// something was written.
    fn copy_navtree_path(&self) -> bool {
        let row = self.tree.rows.get(self.tree.selected);
        let Some(rel) = row.and_then(|r| r.node.id.strip_prefix("files:")) else {
            return false;
        };
        if rel.is_empty() {
            return false;
        }
        let out = match self.active_project_root() {
            Some(root) => {
                let trimmed = root.trim_end_matches(['/', '\\']);
                format!("{trimmed}/{rel}")
            }
            None => rel.to_string(),
        };
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(out.clone())) {
            Ok(()) => {
                tracing::info!(path = %out, "navtree.copy_path → clipboard");
                true
            }
            Err(e) => {
                tracing::warn!(error = %e, "clipboard write failed; nav path not copied");
                false
            }
        }
    }

    /// True if a `files:` node id names a raster image the backend can decode
    /// + crop (ADR 0022). PDFs are excluded — their preview is a rasterized
    /// page, not the `.pdf` file `image.crop` would try to decode.
    fn is_image_node_id(node_id: &str) -> bool {
        let lower = node_id.to_ascii_lowercase();
        [
            ".png", ".jpg", ".jpeg", ".bmp", ".gif", ".webp", ".tif", ".tiff",
        ]
        .iter()
        .any(|e| lower.ends_with(e))
    }

    /// Absolute backend-side path for a `files:<rel>` node id (ADR 0022) —
    /// joins the active workspace's project root so the in-pane LLM (on the
    /// backend) can locate the source. Falls back to the bare relative path.
    fn backend_abs_path(&self, node_id: &str) -> String {
        let rel = node_id.strip_prefix("files:").unwrap_or(node_id);
        match self.active_project_root() {
            Some(root) => format!("{}/{}", root.trim_end_matches(['/', '\\']), rel),
            None => rel.to_string(),
        }
    }

    /// ADR 0022: capture the current image-preview ROI. Fires `image.crop`
    /// against the active workspace; the `ImageCropped` reply pastes a
    /// "look at this" line into the LLM pane. No-op (with a status hint) when
    /// the preview isn't a croppable image.
    fn capture_roi(&mut self) {
        let Some(roi) = self.preview_roi.clone() else {
            self.status = "capture: no image ROI in preview (zoom an image first)".to_string();
            self.window.request_redraw();
            return;
        };
        let name = roi
            .node_id
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(&roi.node_id)
            .to_string();
        if self
            .req_tx
            .send(crate::transport::OutgoingReq::ImageCrop {
                node_id: roi.node_id.clone(),
                x: roi.x,
                y: roi.y,
                w: roi.w,
                h: roi.h,
                workspace_id: self.active_workspace_id.clone(),
            })
            .is_err()
        {
            self.status = "capture: transport closed".to_string();
            self.window.request_redraw();
            return;
        }
        self.status = format!("capturing ROI {}×{} of {} → LLM…", roi.w, roi.h, name);
        self.window.request_redraw();
    }

    /// Open the Ctrl+N new-file prompt (Files mode only). Computes the
    /// directory the file should land in from the cursored row: a directory
    /// row contains the file directly (use its id); a file row's sibling is
    /// the new file (use the file's parent dir id); the root falls back to
    /// `files:`. Returns false (no-op) when the cursor isn't on a `files:`
    /// row — the caller leaves the keystroke for the normal nav handler.
    fn begin_create_file(&mut self) -> bool {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return false;
        };
        if !row.node.id.starts_with("files:") {
            return false;
        }
        // A directory row contains the file directly; a non-directory row
        // (file / other) is a sibling, so the file goes in its parent dir.
        // The files root (`files:`) is itself a directory.
        let dir_node_id = if row.node.kind == "dir" || row.node.id == "files:" {
            row.node.id.clone()
        } else {
            parent_files_node_id(&row.node.id)
        };
        self.nav_prompt = Some(NavPrompt::CreateFile {
            dir_node_id,
            input: String::new(),
        });
        self.status = "new file: ".to_string();
        self.window.request_redraw();
        true
    }

    /// Append a typed character to the active new-file prompt's name buffer.
    /// Path separators are rejected at the source (the backend's
    /// `node_id_to_path` only accepts a bare child segment) so the user
    /// can't type one in; everything else is allowed and validated on Enter.
    fn nav_prompt_push_char(&mut self, c: char) {
        if c == '/' || c == '\\' {
            return;
        }
        if let Some(NavPrompt::CreateFile { input, .. }) = self.nav_prompt.as_mut() {
            input.push(c);
            self.window.request_redraw();
        }
    }

    /// Backspace one char off the active new-file prompt's name buffer.
    fn nav_prompt_backspace(&mut self) {
        if let Some(NavPrompt::CreateFile { input, .. }) = self.nav_prompt.as_mut() {
            input.pop();
            self.window.request_redraw();
        }
    }

    /// Confirm the new-file prompt: validate the name + check for a sibling
    /// collision, then fire a zero-byte `file.write` for the new id. On an
    /// invalid name or collision, surface a status message and keep the
    /// prompt open (nothing is sent). On success, remember the id so the
    /// `file.write` reply can refresh the dir listing, close the prompt, and
    /// show a "creating …" status.
    fn confirm_create_file(&mut self) {
        let (dir_node_id, name) = match self.nav_prompt.as_ref() {
            Some(NavPrompt::CreateFile { dir_node_id, input }) => {
                (dir_node_id.clone(), input.trim().to_string())
            }
            _ => return,
        };
        let new_id = match build_new_file_node_id(&dir_node_id, &name) {
            Ok(id) => id,
            Err(reason) => {
                self.status = format!("new file · {reason}");
                self.window.request_redraw();
                return;
            }
        };
        // Sibling-collision guard: refuse if any existing row already carries
        // the would-be id (a file or dir of that name already lives here).
        if self.tree.rows.iter().any(|r| r.node.id == new_id) {
            self.status = format!("new file · '{name}' already exists");
            self.window.request_redraw();
            return;
        }
        if let Err(e) = self.req_tx.send(OutgoingReq::FileWrite {
            node_id: new_id.clone(),
            content: String::new(),
            expected_version: None,
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, %new_id, "drop file.write for new file — channel closed");
            self.status = "new file · channel closed".to_string();
            self.window.request_redraw();
            return;
        }
        tracing::info!(%new_id, "navtree.create_file → file.write");
        self.pending_created_node_id = Some(new_id);
        self.nav_prompt = None;
        self.status = format!("creating {name}…");
        self.window.request_redraw();
    }

    /// Dismiss the active NavTree prompt without acting. The cancel message is
    /// variant-aware so the user sees which prompt they backed out of.
    fn cancel_nav_prompt(&mut self) {
        self.status = match self.nav_prompt {
            Some(NavPrompt::ConfirmDelete { .. }) => "delete · cancelled".to_string(),
            _ => "new file · cancelled".to_string(),
        };
        self.nav_prompt = None;
        self.window.request_redraw();
    }

    /// Open the Ctrl+D delete-confirm prompt (Files mode only). Targets the
    /// cursored row when it's a `files:` file. Pre-refuses directories in v1
    /// (the backend rejects them with `is_directory`; we don't even open the
    /// prompt) and surfaces a status message instead. Returns false (no-op)
    /// when the cursor isn't on a deletable `files:` file row — the caller
    /// leaves the keystroke for the normal nav handler.
    fn begin_delete_file(&mut self) -> bool {
        let Some(row) = self.tree.rows.get(self.tree.selected) else {
            return false;
        };
        if !row.node.id.starts_with("files:") {
            return false;
        }
        if is_directory_row(&row.node) {
            self.status = "delete: directories not supported yet".to_string();
            self.window.request_redraw();
            return false;
        }
        self.nav_prompt = Some(NavPrompt::ConfirmDelete {
            node_id: row.node.id.clone(),
            label: row.node.label.clone(),
        });
        self.status = format!("delete {}? [y/N]", row.node.label);
        self.window.request_redraw();
        true
    }

    /// Confirm the delete prompt (`y`/`Y`): fire `file.delete` for the
    /// node id, remember it so the reply can refresh the dir listing, close
    /// the prompt, and show a "deleting …" status. On a closed channel,
    /// surface it and leave nothing pending.
    fn confirm_delete_file(&mut self) {
        let (node_id, label) = match self.nav_prompt.as_ref() {
            Some(NavPrompt::ConfirmDelete { node_id, label }) => (node_id.clone(), label.clone()),
            _ => return,
        };
        if let Err(e) = self.req_tx.send(OutgoingReq::FileDelete {
            node_id: node_id.clone(),
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, %node_id, "drop file.delete — channel closed");
            self.status = "delete · channel closed".to_string();
            self.nav_prompt = None;
            self.window.request_redraw();
            return;
        }
        tracing::info!(%node_id, "navtree.delete_file → file.delete");
        self.pending_deleted_node_id = Some(node_id);
        self.nav_prompt = None;
        self.status = format!("deleting {label}…");
        self.window.request_redraw();
    }

    /// Display name for the preview-pane title: the **basename** of the file
    /// the preview pane is *showing* (`preview_node_id_fired`), not the roaming
    /// cursor — so while pinned (C2) the title tracks the pinned file. Only the
    /// filename (not the full relative path) so it stays short enough for the
    /// pane; the full path is still recoverable via Ctrl+C in NavTree.
    /// `None` for non-file previews (sessions / hosts / workspace rows) and
    /// before any preview has fired.
    fn preview_pane_name(&self) -> Option<String> {
        let rel = self
            .preview_node_id_fired
            .as_deref()?
            .strip_prefix("files:")?;
        if rel.is_empty() {
            return None;
        }
        // Full workspace-relative path (per the maintainer) — not just the basename.
        // `middle_truncate` at render time keeps the head + the filename/ext
        // when the title overflows the pane width.
        // Paginated preview (ADR 0021): surface position + the page-turn
        // keys in the title so the affordance is discoverable.
        match self.preview_page {
            Some((page, count)) if count > 1 => Some(format!("{rel} · p {page}/{count} · n/p")),
            _ => Some(rel.to_string()),
        }
    }

    /// Preview-pane size in physical pixels, as the render-fit hint sent
    /// with `preview.get` (ADR 0021) — rasterizing plugins (PDF) produce
    /// the page at display resolution so the GPU samples ~1:1. `None`
    /// before the first layout pass (pane rect still zero), in which case
    /// the plugin falls back to its fixed DPI.
    fn preview_fit_px(&self) -> (Option<u32>, Option<u32>) {
        let w = (self.pane_rects.preview.width as f32 * self.cell_w) as u32;
        let h = (self.pane_rects.preview.height as f32 * self.cell_h) as u32;
        if w == 0 || h == 0 {
            (None, None)
        } else {
            (Some(w), Some(h))
        }
    }

    /// After a zoom change on a paginated preview, re-request the page at
    /// the new on-screen pixel size so rasterized text re-renders crisp
    /// instead of magnifying the fit-sized bitmap (ADR 0021). Only fires
    /// zooming *in* past the current bitmap's detail, with 1.2× hysteresis
    /// so a held/repeated zoom doesn't flood the backend; zooming out just
    /// linear-downsamples the existing higher-res bitmap. The GPU keeps
    /// showing the stretched texture until the sharper reply swaps in.
    fn maybe_reraster_page(&mut self) {
        let Some((page, _count)) = self.preview_page else {
            return;
        };
        let target = self.preview_png_zoom.max(1.0);
        let have = self.preview_page_raster_zoom;
        let pending = self
            .preview_page_raster_pending
            .map(|(_, z)| z)
            .unwrap_or(0.0);
        if target <= have.max(pending) * 1.2 {
            return;
        }
        let Some(node_id) = self.preview_node_id_fired.clone() else {
            return;
        };
        let (Some(fw), Some(fh)) = self.preview_fit_px() else {
            return;
        };
        // The plugin caps the long side at 4096; scaling here just keeps the
        // request honest about what's needed at this zoom.
        let scaled_w = ((fw as f32 * target).round() as u32).clamp(1, 8192);
        let scaled_h = ((fh as f32 * target).round() as u32).clamp(1, 8192);
        if self
            .req_tx
            .send(crate::transport::OutgoingReq::PreviewGet {
                node_id,
                workspace_id: self.active_workspace_id.clone(),
                page: Some(page),
                fit_w: Some(scaled_w),
                fit_h: Some(scaled_h),
            })
            .is_ok()
        {
            self.preview_page_raster_pending = Some((page, target));
        }
    }

    /// Scale the PNG pan offset when the zoom changes so the point at the
    /// centre of the field of view stays put. `pan_px` is measured in
    /// zoomed-canvas pixels (canvas = letterbox × zoom), so the image
    /// fraction off-centre is `pan / (letterbox × zoom)`. Holding that
    /// fraction fixed across a zoom change means pan scales with the zoom
    /// ratio — without this, zooming while panned drifts the view off the
    /// point you were looking at. The render path re-clamps pan to the new
    /// slack, so over-scrolled values self-correct.
    fn scale_png_pan_for_zoom(&mut self, cur: f32, next: f32) {
        if cur <= 0.0 {
            return;
        }
        let ratio = next / cur;
        self.preview_png_pan_px.0 *= ratio;
        self.preview_png_pan_px.1 *= ratio;
    }

    fn save_png_view(&mut self) {
        let Some(dims) = self.preview_png_dims else {
            return;
        };
        let Some(key) = png_cache_key_from_node_id(self.preview_node_id_fired.as_deref(), dims)
        else {
            return;
        };
        self.preview_png_cache
            .insert(key, (self.preview_png_zoom, self.preview_png_pan_px));
    }

    fn render_preview_source(&mut self, mime: &str, bytes: &[u8]) {
        let scale = self.scale * self.text_scale_mult;
        if mime == "image/png" {
            // Paginated document pages (preview_page set from this reply's
            // extras) filter Linear — rasterized text aliases hard under
            // Nearest. Standalone PNGs keep Nearest per the 2026-05-22 ask.
            let sampler = if self.preview_page.is_some() {
                crate::preview::quad::SamplerKind::Linear
            } else {
                crate::preview::quad::SamplerKind::Nearest
            };
            match crate::preview::png::quad_from_png_bytes(
                &self.device,
                &self.queue,
                &self.quad_pipeline,
                bytes,
                sampler,
            ) {
                Ok(q) => {
                    let dims = q.size_px;
                    self.preview_png = Some(q);
                    self.preview_png_dims = Some(dims);
                    if std::mem::take(&mut self.preview_reraster_keep_view) {
                        // Zoom re-raster of the same page (ADR 0021): the new
                        // bitmap is the same page at higher resolution. Fit-to-
                        // pane normalizes source resolution, so keeping zoom/pan
                        // leaves the on-screen view identical — only sharper.
                    } else {
                        // Restore a cached view if we've seen another PNG
                        // of the same size in the same directory (e.g. the
                        // next render in a time-step series). Miss → fit
                        // to pane, centred.
                        let cache_key =
                            png_cache_key_from_node_id(self.preview_node_id_fired.as_deref(), dims);
                        let restored = cache_key
                            .as_ref()
                            .and_then(|k| self.preview_png_cache.get(k))
                            .copied();
                        match restored {
                            Some((zoom, pan)) => {
                                self.preview_png_zoom = zoom;
                                self.preview_png_pan_px = pan;
                            }
                            None => {
                                self.preview_png_zoom = 1.0;
                                self.preview_png_pan_px = (0.0, 0.0);
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "preview-png decode failed"),
            }
        } else if mime.starts_with("application/vnd.sot.tokens+json") {
            match serde_json::from_slice::<TokensPayload>(bytes) {
                Ok(payload) => {
                    let pairs: Vec<(String, String)> = payload
                        .spans
                        .into_iter()
                        .map(|s| (s.text, s.kind))
                        .collect();
                    self.preview_md = MarkdownPreview::new_tokens(
                        self.text.font_system_mut(),
                        &pairs,
                        self.md_rect_px.w.max(1.0),
                        scale,
                    );
                    self.preview_png = None;
                    self.preview_svg = None;
                    self.preview_scroll = 0;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "tokens+json decode failed");
                }
            }
        } else if bytes.len() > PREVIEW_TEXT_CAP || looks_binary(bytes) {
            // Oversized or binary payload: never feed it to comrak / the text
            // shaper — doing so on the single render thread freezes every pane
            // (this is what a multi-GB `.h5` whose backend fallback returned
            // raw bytes did). Show a one-line summary instead. The PNG/tokens
            // branches above still handle their real binary formats.
            let kind = if looks_binary(bytes) {
                "binary"
            } else {
                "large"
            };
            let msg = format!(
                "{kind} file — {} bytes\n\nNot previewed as text (mime: {mime}).",
                bytes.len()
            );
            self.preview_md = MarkdownPreview::new_plain(
                self.text.font_system_mut(),
                &msg,
                self.md_rect_px.w.max(1.0),
                scale,
            );
            self.preview_png = None;
            self.preview_svg = None;
            self.preview_scroll = 0;
        } else if mime == "text/markdown" || mime == "text/x-markdown" {
            if let Ok(s) = std::str::from_utf8(bytes) {
                let math_metrics = self.build_math_metrics();
                let figure_metrics = self.build_figure_metrics();
                self.preview_md = MarkdownPreview::new(
                    self.text.font_system_mut(),
                    s,
                    self.md_rect_px.w.max(1.0),
                    self.md_rect_px.h.max(1.0),
                    scale,
                    &math_metrics,
                    &figure_metrics,
                    &self.highlight_service,
                    &self.markdown_token_cache,
                );
                self.preview_png = None;
                self.preview_svg = None;
                self.preview_scroll = 0;
                // Fire math.render for any `$$...$$` blocks the walk
                // discovered. Replies populate `math_cache` and the
                // next paint pulls the SVG in via A3.
                self.dispatch_pending_math();
                // Fire figure.get for any `![](url)` regions. Replies
                // populate `figure_cache` and the next paint draws the
                // bitmap over the FFFC placeholder the walk reserved.
                self.dispatch_pending_figures();
                // Fire markdown.tokenize for any Julia fence that
                // didn't hit the per-fence cache. Replies overlay onto
                // tree-sitter's base on the next reflow.
                self.dispatch_pending_markdown_tokens();
            }
        } else if let Ok(s) = std::str::from_utf8(bytes) {
            self.preview_md = MarkdownPreview::new_plain(
                self.text.font_system_mut(),
                s,
                self.md_rect_px.w.max(1.0),
                scale,
            );
            self.preview_png = None;
            self.preview_svg = None;
            self.preview_scroll = 0;
        } else {
            tracing::debug!(
                %mime,
                len = bytes.len(),
                "preview blob is not UTF-8 and not an image — skipping"
            );
        }
    }

    /// Bump the runtime font-scale multiplier and propagate. Recomputes
    /// chrome cell metrics, updates the TextLayer's per-line metrics,
    /// and replays the cached preview source so an open .jl / .md
    /// reflows at the new size. Clamped to a sane range so the user
    /// can't soft-lock the chrome by zooming to 0.01.
    fn apply_text_scale(&mut self, mult: f32) {
        self.text_scale_mult = mult.clamp(0.5, 3.0);
        let s = self.scale * self.text_scale_mult;
        self.cell_h = BASE_CELL_H * s;
        self.chrome_origin_x = BASE_CHROME_ORIGIN_X * s;
        self.chrome_origin_y = BASE_CHROME_ORIGIN_Y * s;
        self.text
            .set_metrics(cosmic_text::Metrics::new(14.0 * s, 18.0 * s));
        // Re-measure monospace advance at the new metrics so the cell
        // grid still matches cosmic-text's actual glyph positioning
        // after a runtime font-size change. Fall back to BASE_CELL_W * s
        // if shape fails (no monospace font installed).
        self.cell_w = self.text.monospace_advance().unwrap_or(BASE_CELL_W * s);
        // Re-derive the chrome grid against the new cell metrics —
        // without this the cell count (cols, rows) stays at the old
        // value while each cell paints at the new (bigger) size, and
        // content extends past the wgpu surface edge. The window
        // doesn't resize; the grid does.
        let (cols, rows) = cell_grid_for(
            self.config.width,
            self.config.height,
            self.cell_w,
            self.cell_h,
            self.chrome_origin_x,
            self.chrome_origin_y,
        );
        self.terminal.backend_mut().resize(cols, rows);
        let _ = self
            .terminal
            .resize(ratatui::layout::Rect::new(0, 0, cols, rows));
        // Replay the cached preview source — without this the open
        // file would stay at its original scale until the user
        // navigated to a different file.
        if let Some((mime, bytes)) = self.preview_src.clone() {
            self.render_preview_source(&mime, &bytes);
        }
        // Reflow the help overlay at the new scale if it's open.
        if self.help_open {
            self.rebuild_help_overlay();
        }
    }

    /// Fire `math.render` for every block in the latest markdown
    /// preview that isn't already cached or in flight. Called after
    /// `preview_md` is (re)built. Idempotent — repeated calls with
    /// the same blocks do nothing once the cache is warm.
    fn dispatch_pending_math(&mut self) {
        let blocks = self.preview_md.media_blocks.clone();
        for block in blocks {
            let crate::preview::markdown::MediaBlock::Math { latex, display } = block else {
                continue;
            };
            let key = (latex.clone(), display);
            if self.math_cache.contains_key(&key) || self.math_pending.contains(&key) {
                continue;
            }
            if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::MathRender {
                latex: latex.clone(),
                display,
            }) {
                tracing::warn!(error = %e,
                    "drop math.render request — channel closed");
                continue;
            }
            self.math_pending.insert(key);
        }
    }

    /// Build (or rebuild) the per-table cosmic-text Buffers that host
    /// wide GFM tables at natural width — see `TableBufferEntry`
    /// docstring. Compares the current `media_blocks` Table sources
    /// against the cached `rendered` strings; if any differ (or count
    /// changed), the whole `table_buffers` Vec is rebuilt and the
    /// horizontal scroll resets to 0 so navigation between docs starts
    /// fresh. No-op when buffers are already in sync — typical steady
    /// state across redraws.
    fn ensure_table_buffers(&mut self) {
        use crate::preview::markdown::MediaBlock;
        // Snapshot the (rendered, font_px, line_h_px) of every Table in
        // current source order. Snapshot avoids the &mut self / &self
        // borrow conflict when we walk media_blocks then build buffers.
        let snapshots: Vec<(String, f32, f32)> = self
            .preview_md
            .media_blocks
            .iter()
            .filter_map(|b| match b {
                MediaBlock::Table {
                    rendered,
                    font_px,
                    line_h_px,
                    ..
                } => Some((rendered.clone(), *font_px, *line_h_px)),
                _ => None,
            })
            .collect();
        let in_sync = snapshots.len() == self.table_buffers.len()
            && snapshots
                .iter()
                .zip(self.table_buffers.iter())
                .all(|((r, _, _), e)| r == &e.rendered);
        if in_sync {
            return;
        }
        self.table_buffers.clear();
        self.md_table_scroll_px = 0.0;
        for (rendered, font_px, line_h_px) in snapshots {
            let metrics = cosmic_text::Metrics::new(font_px, line_h_px);
            let mut buf = cosmic_text::Buffer::new(self.text.font_system_mut(), metrics);
            // 10_000 px is wider than any realistic GFM table so the
            // box-drawing lines lay out without soft-wrap. `None` for
            // height matches the main preview buffer's "extend past
            // the rect" policy.
            buf.set_size(self.text.font_system_mut(), Some(10_000.0), None);
            let attrs = cosmic_text::Attrs::new()
                .family(cosmic_text::Family::Monospace)
                .metrics(metrics);
            buf.set_text(
                self.text.font_system_mut(),
                &rendered,
                attrs,
                cosmic_text::Shaping::Advanced,
            );
            buf.shape_until_scroll(self.text.font_system_mut(), false);
            let mut natural_w_px: f32 = 0.0;
            for run in buf.layout_runs() {
                if run.line_w > natural_w_px {
                    natural_w_px = run.line_w;
                }
            }
            self.table_buffers.push(TableBufferEntry {
                rendered,
                buffer: buf,
                natural_w_px,
            });
        }
    }

    /// Drain `preview_md.pending_token_fences` and fire
    /// `OutgoingReq::MarkdownTokenize` for each `(lang, source_hash)`
    /// that's not already in cache or in flight. Replies route through
    /// `IncomingEvt::MarkdownTokens`, populate the per-fence cache, and
    /// trigger a reflow so the next redraw consumes the overlay.
    fn dispatch_pending_markdown_tokens(&mut self) {
        let pending = std::mem::take(&mut self.preview_md.pending_token_fences);
        for (lang, source_hash, source) in pending {
            let key = (lang.clone(), source_hash);
            if self.markdown_token_cache.contains_key(&key)
                || self.markdown_token_pending.contains(&key)
            {
                continue;
            }
            if let Err(e) = self
                .req_tx
                .send(crate::transport::OutgoingReq::MarkdownTokenize {
                    lang: lang.clone(),
                    source_hash,
                    source,
                })
            {
                tracing::warn!(error = %e,
                    "drop markdown.tokenize request — channel closed");
                continue;
            }
            self.markdown_token_pending.insert(key);
        }
    }

    /// Walk `preview_md.buffer`'s laid-out runs looking for the FFFC
    /// (OBJECT REPLACEMENT CHARACTER) placeholders that
    /// `preview/markdown.rs` emitted for `$$…$$` / `$…$` / `![](…)`
    /// regions. Returns one entry per FFFC glyph, in source order,
    /// paired with the screen rect to paint into (preview_rect-relative
    /// + scroll-adjusted). The order matches `preview_md.media_blocks`
    /// so callers can zip the two by index without re-parsing the
    /// buffer text.
    ///
    /// `preview_scroll_px` is applied so a placeholder that's scrolled
    /// off-screen produces a rect with `y` < `preview_rect.y` — caller
    /// culls.
    fn collect_media_paint_targets(
        &self,
        preview_rect: ScreenRect,
        preview_scroll_px: f32,
    ) -> Vec<(usize, ScreenRect)> {
        use crate::preview::markdown::MediaBlock;
        let mut out = Vec::new();
        if self.preview_md.media_blocks.is_empty() {
            return out;
        }
        let body_em_px = self.preview_md.body_em().max(1.0);
        let mut fffc_idx: usize = 0;
        for run in self.preview_md.buffer.layout_runs() {
            for g in run.glyphs.iter() {
                // The glyph's start/end span the source text byte
                // range it represents; checking the slice avoids
                // having to know cosmic-text's glyph-id mapping for
                // FFFC.
                let end = g.end.min(run.text.len());
                if end <= g.start {
                    continue;
                }
                if &run.text[g.start..end] != "\u{FFFC}" {
                    continue;
                }
                let Some(block) = self.preview_md.media_blocks.get(fffc_idx) else {
                    fffc_idx += 1;
                    continue;
                };
                let rect = match block {
                    MediaBlock::Math { display: true, .. }
                    | MediaBlock::Figure { .. }
                    | MediaBlock::Table { .. } => {
                        // Block-level (display math / figure / table) —
                        // claim the full row the FFFC reserved. The
                        // paint pass centres the bitmap inside it for
                        // math/figure; for tables, the chrome hosts a
                        // separate cosmic-text buffer at natural width
                        // and lets TextBounds clip the overflow to the
                        // preview pane (Path 1 of (e)).
                        ScreenRect {
                            x: preview_rect.x,
                            y: preview_rect.y + run.line_top - preview_scroll_px,
                            w: preview_rect.w,
                            h: run.line_height,
                        }
                    }
                    MediaBlock::Math {
                        display: false,
                        latex,
                    } => {
                        // Inline: anchor at the FFFC glyph's x position, size
                        // to the cached SVG's natural pixel dimensions, and
                        // baseline-align using MathJax's `vertical-align`
                        // (negative ex → SVG bottom hangs N px below the
                        // text baseline). The cache lookup might miss on the
                        // first paint after a reflow if the SVG was dropped
                        // between then and now — fall back to a single-line
                        // anchor rect so we don't paint at (0, 0).
                        let key = (latex.clone(), false);
                        let entry = self.math_cache.get(&key);
                        let (svg_w_px, svg_h_px, drop_px) = match entry {
                            Some(e) => {
                                let w = e
                                    .width_ex
                                    .map(|v| v * MATHJAX_EX_FACTOR * body_em_px)
                                    .unwrap_or(body_em_px * 2.0);
                                let h = e
                                    .height_ex
                                    .map(|v| v * MATHJAX_EX_FACTOR * body_em_px)
                                    .unwrap_or(run.line_height);
                                let d = e
                                    .vertical_align_ex
                                    .map(|v| (-v) * MATHJAX_EX_FACTOR * body_em_px)
                                    .unwrap_or(0.0);
                                (w, h, d)
                            }
                            None => (body_em_px * 2.0, run.line_height, 0.0),
                        };
                        let baseline_y = preview_rect.y + run.line_y - preview_scroll_px;
                        let svg_bottom_y = baseline_y + drop_px;
                        let svg_top_y = svg_bottom_y - svg_h_px;
                        ScreenRect {
                            x: preview_rect.x + g.x,
                            y: svg_top_y,
                            w: svg_w_px,
                            h: svg_h_px,
                        }
                    }
                };
                out.push((fffc_idx, rect));
                fffc_idx += 1;
            }
        }
        out
    }

    /// (Re)build the keybindings help overlay buffer at the current
    /// preview-pane width. Static content — a monospace, focus-grouped
    /// cheat-sheet of the major bindings — so it's cheap to rebuild on
    /// open / resize. Bindings here mirror the handlers in this file; keep
    /// them in sync when adding a key.
    fn rebuild_help_overlay(&mut self) {
        const HELP: &str = "\
  Ship of Tools — key bindings    ( ? or Esc to close )

  Move between panes & sessions   (works in any focus)
    Ctrl+← → ↑ ↓    move focus between panes
    Shift+← →       switch session (cycle workspace)
    Alt+=  maximize pane            Esc  restore (un-maximize)
    Ctrl+Shift+S   selfie — save a PNG of the whole window

  Drawers   (works in any focus)
    Ctrl+J  REPL          Ctrl+T  terminal          Ctrl+M  monitor

  Modes (Nav focus)
    f   Files          m   Modules
    s   Sessions       h   Hosts

  Navigate (Nav focus)
    ↑ ↓   move cursor     → / ←   expand / collapse     Enter   open / pick

  Files (Nav focus)
    d   download cursored file to your machine
    u   upload a local file (OS picker) into the cursored folder
    o   open       (html→browser · .jl→Pluto · video→browser · .qmd→quick render)
    O   open + run code chunks  (.qmd execute)
    W   open the built docs site in browser  (deep-links a built docs page)
    r   run .jl in a fresh REPL     R   run .jl in current REPL
    p   pin preview                 Ctrl+C  copy file path
    Ctrl+N  new file                Ctrl+D  delete file
    .   show / hide hidden files (dotfiles) in the tree

  Sessions (Nav focus, s)
    Enter on a session row     switch to it
    Enter on the '+' row       open the new-session folder picker, then:
        Enter        create with a CLAUDE CODE agent
        Shift+Enter  folder only — shell, no agent
        Ctrl+Enter   create with a CODEX agent
    Shift+D   destroy the cursored session (press D again to confirm)

  Preview focus
    ↑ ↓ scroll     h l 0  scroll wide table     y  copy code blocks
    image:  Shift+↑ ↓ (or + / -)  zoom    r / 0  reset    ← →  pan
    PgDn/PgUp or n/p   next / prev page  (paginated previews, e.g. PDF)
    e   edit concept annotation   (Ctrl+S save · Esc discard)
    o / O / W   open the SHOWN file  (same as Nav — acts on what you see)

  REPL drawer
    r on a .jl   run in a FRESH repl (restarts the process in that project)
    R on a .jl   run in the current repl        Ctrl+L   clear scrollback

  LLM pane (mouse)
    drag           select text (multi-line)   Ctrl+Shift+C  copy selection
    click outside  clear selection            wheel         scroll history

  Quit:  q  (or Ctrl+Q)
";
        let width = self.md_rect_px.w.max(1.0);
        let scale = self.scale * self.text_scale_mult;
        self.preview_help = Some(MarkdownPreview::new_plain(
            self.text.font_system_mut(),
            HELP,
            width,
            scale,
        ));
    }

    /// Rebuild the ADR 0030 protocol-mismatch overlay buffer from
    /// `protocol_mismatch`. Mirrors `rebuild_help_overlay` — a plain monospace
    /// buffer shaped to the preview width — so it reuses the same overlay paint
    /// path. No-op (clears the buffer) when no mismatch is set.
    fn rebuild_fatal_overlay(&mut self) {
        let Some(body) = self.protocol_mismatch.clone() else {
            self.preview_fatal = None;
            return;
        };
        let width = self.md_rect_px.w.max(1.0);
        let scale = self.scale * self.text_scale_mult;
        self.preview_fatal = Some(MarkdownPreview::new_plain(
            self.text.font_system_mut(),
            &body,
            width,
            scale,
        ));
    }

    /// Rebuild `preview_edit` from the active edit buffer. Layout:
    ///   - read-only frontmatter header (if any), prefixed line-by-line
    ///     with `│ ` so the user reads it as a sidebar
    ///   - blank line separator
    ///   - editable body with `█` injected at the cursor byte
    ///   - blank line + status footer ("modified" if dirty; modal
    ///     prompt when `confirm_discard` is up)
    /// Called whenever a key mutates the buffer; cheap enough for a
    /// few-KB annotation. When `edit_state` is `None`, clears the
    /// preview so the next render falls back to the read-only path.
    fn rebuild_edit_preview(&mut self) {
        let Some(edit) = self.edit_state.as_ref() else {
            self.preview_edit = None;
            return;
        };
        // Compose styled spans: header, body (cursor block + optional
        // selection tint), footer. Concatenated text is what renders; the
        // `bool` flags the selected run so `new_plain_spans` tints it.
        let mut spans: Vec<(String, bool)> = Vec::new();
        if let Some(header) = &edit.header {
            let mut h = String::new();
            for line in header.lines() {
                h.push_str("│ ");
                h.push_str(line);
                h.push('\n');
            }
            h.push('\n');
            spans.push((h, false));
        }
        let body = edit.buf.body();
        let cur = edit.buf.cursor();
        const CURSOR: &str = "\u{2588}";
        let has_sel = edit.buf.selection_range().is_some();
        match edit.buf.selection_range() {
            // Cursor sits at one end of the range (a = start, b = end). Place
            // the cursor block on its side; tint the selected run amber.
            Some((a, b)) if cur <= a => {
                spans.push((body[..a].to_string(), false));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[a..b].to_string(), true));
                spans.push((body[b..].to_string(), false));
            }
            Some((a, b)) => {
                spans.push((body[..a].to_string(), false));
                spans.push((body[a..b].to_string(), true));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[b..].to_string(), false));
            }
            None => {
                spans.push((body[..cur].to_string(), false));
                spans.push((CURSOR.to_string(), false));
                spans.push((body[cur..].to_string(), false));
            }
        }
        // Footer: blank line + status. Modal overrides everything — when the
        // user is confirming, that's the only thing they should read.
        // Priority: stale > discard-confirm > selection > modified > clean.
        let mut footer = String::from("\n\n");
        if edit.stale_banner {
            footer.push_str(
                "── STALE: file changed on disk since you started editing.  [r] reload (discard your edits)  [k] keep editing (next save will fail again) ──",
            );
        } else if edit.confirm_discard {
            footer.push_str(
                "── DISCARD UNSAVED EDITS?  [y]/[Esc] throw away changes  [n] keep editing ──",
            );
        } else if has_sel {
            footer.push_str("── edit mode · selection · Ctrl+C copy · Ctrl+X cut · Ctrl+S save ──");
        } else if edit.is_dirty() {
            footer.push_str("── edit mode · modified · Ctrl+S save · Esc discard ──");
        } else {
            footer.push_str("── edit mode · clean · Ctrl+S save · Esc exit ──");
        }
        spans.push((footer, false));
        let width = self.concept_rect_px.w.max(1.0);
        let scale = self.scale * self.text_scale_mult;
        self.preview_edit = Some(MarkdownPreview::new_plain_spans(
            self.text.font_system_mut(),
            &spans,
            width,
            scale,
        ));
    }

    fn submit_repl_input(&mut self) {
        let code = std::mem::take(&mut self.repl_input);
        if code.trim().is_empty() {
            return;
        }
        // Submit exits any active history walk — the saved buffer is
        // discarded because the user committed to this line.
        self.history_pos = None;
        self.history_saved = None;
        self.repl_eval_counter = self.repl_eval_counter.saturating_add(1);
        let eval_id = self.repl_eval_counter;
        // Tag the eval with the workspace it belongs to so a `ReplEvalDone`
        // reply arriving after a swap routes back to the right log.
        let workspace_key = self.current_workspace_key();
        self.eval_id_workspace
            .insert(eval_id, workspace_key.clone());
        // Bound the scrollback at a reasonable cap so a long session
        // doesn't accumulate forever. Trim from the front.
        if self.repl_log.len() >= 256 {
            let excess = self.repl_log.len() - 255;
            self.repl_log.drain(0..excess);
        }
        let pkg_mode = self.repl_pkg_mode;
        self.repl_log.push(ReplEntry {
            eval_id,
            code: code.clone(),
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight: true,
            pkg_mode,
        });
        let mode = if pkg_mode {
            Some("pkg".to_string())
        } else {
            None
        };
        if let Err(e) = self.req_tx.send(crate::transport::OutgoingReq::ReplEval {
            eval_id,
            code,
            mode,
            workspace_id: self.active_workspace_id.clone(),
        }) {
            tracing::warn!(error = %e, eval_id, "drop repl.eval request — channel closed");
            // Mark the entry done with an error frame so the user sees
            // why nothing happened.
            if let Some(entry) = self.repl_log.iter_mut().find(|e| e.eval_id == eval_id) {
                entry.in_flight = false;
                entry.frames.push(sot_protocol::ReplFrame::Error {
                    message: format!("transport channel closed: {e}"),
                    stacktrace: Vec::new(),
                });
            }
        }
    }

    fn history_step_back(&mut self) -> Option<String> {
        history_step_back(
            &self.repl_log,
            &mut self.history_pos,
            &mut self.history_saved,
            &self.repl_input,
        )
    }

    fn history_step_forward(&mut self) -> Option<String> {
        history_step_forward(
            &self.repl_log,
            &mut self.history_pos,
            &mut self.history_saved,
        )
    }

    fn drain_events(&mut self) {
        while let Ok(evt) = self.evt_rx.try_recv() {
            match evt {
                crate::transport::IncomingEvt::Connected {
                    session_id,
                    revision,
                    host,
                    project_root,
                } => {
                    // Cache host + daemon root basename so the chrome can
                    // rebuild the connection status every time the active
                    // workspace changes — not just at hello time. Strip
                    // FQDN to short hostname ("myhost", not "myhost.example
                    // .org") so it fits the nav status row.
                    self.host = host
                        .as_deref()
                        .and_then(|h| h.split('.').next())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                    self.daemon_root_basename = project_root.as_deref().and_then(|p| {
                        p.rsplit(['/', '\\'])
                            .next()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    });
                    self.daemon_project_root = project_root.clone();
                    self.last_revision = revision;
                    let _ = session_id;
                    // ADR 0030 §2: a clean hello means the protocol skew (if
                    // any) is resolved — clear the blocking "update needed"
                    // overlay so the chrome returns to normal.
                    self.protocol_mismatch = None;
                    self.preview_fatal = None;
                    // An in-flight file.upload can't survive a transport reset —
                    // its chunk/ack loop is broken and any daemon-side partial is
                    // orphaned. Clear the stranded state so `u` isn't blocked by a
                    // ghost "upload · already in progress" (an oversized-chunk
                    // frame that reset the transport used to strand it forever).
                    if self.upload.take().is_some() {
                        self.status =
                            "upload interrupted by reconnect — press u to retry".to_string();
                        self.notify_sticky_until = Some(std::time::Instant::now() + NOTIFY_STICKY);
                    }
                    self.rebuild_connection_status();
                    // Prime the slug→label cache so the status line shows
                    // the friendly workspace label even on a fresh launch
                    // that resumed into a non-default workspace (Sessions
                    // mode isn't visited; the reply just refreshes
                    // `workspace_labels` and re-renders the status). Cheap;
                    // the reply is small.
                    let _ = self
                        .req_tx
                        .send(crate::transport::OutgoingReq::WorkspaceList);
                    // B5 resume: the transport's hello-time TreeRoot always
                    // requests "files" against the *default* workspace. If
                    // we restored into a different mode — or restored into
                    // Files but with an `active_workspace_id` set (ADR
                    // 0014) — fire the right request now.
                    match self.mode {
                        Mode::Sessions => {
                            let _ = self
                                .req_tx
                                .send(crate::transport::OutgoingReq::WorkspaceList);
                        }
                        Mode::Modules => {
                            let _ = self
                                .req_tx
                                .send(crate::transport::OutgoingReq::ProjectScan {
                                    workspace_id: self.active_workspace_id.clone(),
                                });
                        }
                        Mode::Files => {
                            if self.active_workspace_id.is_some() {
                                let _ = self.req_tx.send(crate::transport::OutgoingReq::TreeRoot {
                                    mode: "files".to_string(),
                                    workspace_id: self.active_workspace_id.clone(),
                                });
                            }
                        }
                        Mode::Hosts => {
                            // ADR 0015: no backend round-trip — the
                            // hosts tree is sourced from `hosts.toml`
                            // on the frontend side. Populate once on
                            // resume; subsequent `h` re-entries call
                            // `populate_hosts_tree` directly.
                            self.populate_hosts_tree();
                        }
                    }
                    // Reconnect after laptop sleep / SSH-tunnel drop:
                    // the backend's tmux master + workspace state survive,
                    // but the per-connection pty reader/writer pair died
                    // with the old transport. Re-fire PtyOpen so the BL
                    // pane resumes streaming bytes instead of sitting on
                    // its pre-suspend buffer.
                    if let Some(target) = self.bl_pane_target.clone() {
                        let (cols, rows) = self.pty_size.unwrap_or((80, 24));
                        let _ = self.req_tx.send(crate::transport::OutgoingReq::PtyOpen {
                            cols,
                            rows,
                            target: Some(target),
                            // #5 guard: a reconnect re-attach (sleep /
                            // tunnel drop) is NOT a user switch — re-stream
                            // the existing target, don't yank the foreground.
                            user_switch: false,
                        });
                    }
                    // And re-fire preview for the currently-cursored node
                    // so any file changes that landed while we were
                    // disconnected actually show up. preview_node_id_fired
                    // is the source of truth for "what the preview pane is
                    // showing right now".
                    if let Some(node_id) = self.preview_node_id_fired.clone() {
                        let (fit_w, fit_h) = self.preview_fit_px();
                        let _ = self.req_tx.send(crate::transport::OutgoingReq::PreviewGet {
                            node_id,
                            workspace_id: self.active_workspace_id.clone(),
                            // Hold the page across the reconnect — a
                            // blip shouldn't yank a paginated preview
                            // back to page 1.
                            page: self.preview_page.map(|(p, _)| p),
                            fit_w,
                            fit_h,
                        });
                    }
                }
                crate::transport::IncomingEvt::Disconnected { reason } => {
                    self.status = format!("disconnected · {reason}");
                }
                crate::transport::IncomingEvt::ProtocolMismatch { message } => {
                    // ADR 0030 §2: hard FE/BE version skew. Latch the blocking
                    // overlay (rebuilt lazily in the draw once md_rect_px is
                    // known, so it wraps to the real preview width) and mirror
                    // a short line to the status bar.
                    self.protocol_mismatch = Some(message);
                    self.preview_fatal = None;
                    self.status = "protocol mismatch · update needed (see preview)".to_string();
                }
                crate::transport::IncomingEvt::TreeRoot {
                    workspace_id,
                    root,
                    children,
                } => {
                    // ADR 0014: drop a tree.root reply for a workspace we've
                    // since switched away from. An in-flight reply (or the
                    // connect-time default fetch) would otherwise clobber the
                    // now-active workspace's tree, leaving nav + preview
                    // rooted on the old project while the LLM pane and
                    // workspace selector already moved on (the nav/llm
                    // desync reported 2026-05-29).
                    if workspace_id != self.active_workspace_id {
                        continue;
                    }
                    // Drop the hello-time tree.root reply when we
                    // resumed into a non-Files mode — the file tree
                    // would otherwise clobber the Sessions / Hosts /
                    // (future) other-mode root the chrome already
                    // built locally. Files mode is the only one whose
                    // root *is* the tree.root response; everything
                    // else builds its tree from a different source.
                    if !matches!(self.mode, Mode::Files | Mode::Modules) {
                        continue;
                    }
                    self.tree.set_root(root, children);
                    // Restore the nav cursor persisted across an ADR-0017
                    // relaunch, best-effort: select the saved node id if it's
                    // present in the freshly loaded tree (one-shot, gated by
                    // the workspace check above so it only lands in the
                    // matching workspace's tree). A deeply-collapsed node that
                    // isn't loaded yet just leaves the default cursor. A
                    // CLI --start-selected below still overrides this.
                    let mut resume_landed = false;
                    if let Some((id, scroll)) = self.pending_resume_nav.take() {
                        if let Some(idx) = self.tree.rows.iter().position(|r| r.node.id == id) {
                            self.tree.selected = idx;
                            self.tree_scroll = scroll;
                            resume_landed = true;
                        }
                    }
                    // Fresh session (no resume landed): default the first
                    // Files-mode cursor to the project README so the preview
                    // opens onto rendered docs instead of the root row.
                    // The one-shot is consumed on the FIRST Files tree.root
                    // either way, so later refreshes never yank the cursor.
                    // `--start-selected` below still overrides.
                    // `--capture-preview` runs skip the README default: it
                    // moves the cursor, and the cursor-driven readme fetch
                    // then lands after (and clobbers) the captured preview —
                    // the first-row "pretend we already fired" suppression
                    // below only holds while the cursor stays on row 0.
                    let ws_key = self.current_workspace_key();
                    let readme_default = matches!(self.mode, Mode::Files)
                        && self.capture_preview.is_none()
                        && self.nav_readme_defaulted.insert(ws_key);
                    if !resume_landed && readme_default {
                        if let Some(idx) = self
                            .tree
                            .rows
                            .iter()
                            .position(|r| r.node.label.eq_ignore_ascii_case("readme.md"))
                        {
                            self.tree.selected = idx;
                        }
                    }
                    // Only consume the start-selected one-shot if this
                    // event matches our startup mode; otherwise the files-
                    // mode `tree.root` that always fires at connect would
                    // eat the selection meant for the modules tree.
                    if matches!(self.mode, Mode::Files) {
                        if let Some(n) = self.pending_initial_selection.take() {
                            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                        }
                        if let Some(rel) = self.capture_preview.take() {
                            let node_id = format!("files:{rel}");
                            tracing::info!(%node_id, "firing --capture-preview");
                            let (fit_w, fit_h) = self.preview_fit_px();
                            if let Err(e) =
                                self.req_tx.send(crate::transport::OutgoingReq::PreviewGet {
                                    node_id: node_id.clone(),
                                    workspace_id: None,
                                    page: None,
                                    fit_w,
                                    fit_h,
                                })
                            {
                                tracing::warn!(error = %e, %node_id, "drop --capture-preview request — channel closed");
                            }
                            // Record the REAL fired node id. The cursor-
                            // driven auto-fire race this used to paper over
                            // (by pretending the root row was fired) is now
                            // killed at the source — maybe_fire_preview
                            // stands down while capture_preview_armed. The
                            // honest id matters: the pane title and the
                            // ADR-0021 page transport (PgDn/PgUp/n/p re-fire
                            // the *shown* node) both read it — the root-row
                            // lie made a page turn re-fetch the root preview
                            // and clobber the captured node.
                            self.preview_node_id_fired = Some(node_id);
                        }
                        // #4 fix (cursor-reveal-on-switch): a preview driven via
                        // a workspace switch armed a one-shot reveal before this
                        // workspace's rows existed. They're loaded now — land the
                        // cursor on the driven file. `drive_reveal_step` lands a
                        // top-level row directly and expands ancestors for a
                        // nested one. Runs after the resume/README cursor
                        // defaults above so the explicit switch-reveal wins.
                        if let Some(node_id) = self.pending_switch_reveal.take() {
                            // Hold the per-frame preview-follow off the just-applied
                            // README/default cursor while this deep reveal lands, so
                            // `maybe_fire_preview` can't clobber the driven badge
                            // preview with README (the post-relaunch badge-consume
                            // race — two repros 2026-06-30). Mirrors
                            // `drive_same_ws_open`; cleared on landing.
                            if !self.tree.rows.iter().any(|r| r.node.id == node_id) {
                                self.driven_preview_hold_cursor = self
                                    .tree
                                    .rows
                                    .get(self.tree.selected)
                                    .map(|r| r.node.id.clone());
                            }
                            self.pending_reveal = Some(node_id);
                            self.drive_reveal_step();
                        }
                    }
                }
                crate::transport::IncomingEvt::TreeChildren {
                    workspace_id,
                    parent_id,
                    children,
                } => {
                    // Same workspace guard as TreeRoot: a lazy-expand reply
                    // for a workspace we've left must not splice into the
                    // current workspace's tree.
                    if workspace_id != self.active_workspace_id {
                        continue;
                    }
                    self.tree.apply_children(&parent_id, children);
                    // Advance an in-flight deep-path reveal: this splice may have
                    // just made the next ancestor (or the target row) visible.
                    // No-op when no reveal is armed.
                    self.drive_reveal_step();
                }
                crate::transport::IncomingEvt::ProjectScan {
                    project_root,
                    package_name,
                    entry_file,
                    modules,
                } => {
                    tracing::info!(
                        ?project_root,
                        ?package_name,
                        ?entry_file,
                        module_count = modules.len(),
                        type_count = modules.iter().map(|m| m.types.len()).sum::<usize>(),
                        fn_count = modules.iter().map(|m| m.functions.len()).sum::<usize>(),
                        "project.scan reply"
                    );
                    self.scan_project_root = project_root;
                    let rows = scan_to_tree_rows(&modules);
                    self.tree.set_flat(rows);
                    if matches!(self.mode, Mode::Modules) {
                        if let Some(n) = self.pending_initial_selection.take() {
                            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                        }
                    }
                }
                crate::transport::IncomingEvt::ModulesList { modules } => {
                    // Synthesize TreeNodes so Modules-mode reuses the same
                    // TreeView rendering as Files-mode. `path` from Linux's
                    // 4e1c8c0 rides along on `payload.path` so the keyboard
                    // handler can issue `file.parse` for module expansion
                    // without re-querying the kernel. Built-ins (no path)
                    // stay unexpandable.
                    let root = TreeNode {
                        id: "modules:".to_string(),
                        label: "modules".to_string(),
                        kind: "modules".to_string(),
                        has_children: !modules.is_empty(),
                        badges: Vec::new(),
                        payload: Default::default(),
                    };
                    let children = modules
                        .into_iter()
                        .map(|m| {
                            let mut payload = serde_json::Map::new();
                            if let Some(p) = m.path.as_ref() {
                                payload.insert(
                                    "path".to_string(),
                                    serde_json::Value::String(p.clone()),
                                );
                            }
                            TreeNode {
                                id: format!("modules:{}", m.name),
                                label: m.name,
                                kind: "module".to_string(),
                                has_children: m.path.is_some(),
                                badges: Vec::new(),
                                payload,
                            }
                        })
                        .collect();
                    self.tree.set_root(root, children);
                    if matches!(self.mode, Mode::Modules) {
                        if let Some(n) = self.pending_initial_selection.take() {
                            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                        }
                    }
                }
                crate::transport::IncomingEvt::FileParseFailed { path } => {
                    // Record the failure; the retry gate in
                    // maybe_fire_concept_read re-arms after a backoff. Do
                    // NOT un-latch `file_parse_fired` here — an instant
                    // un-latch let the redraw loop re-fire every frame
                    // against a fast-failing kernel (the ~4.7k req/s storm).
                    let e = self
                        .file_parse_retry
                        .entry(path)
                        .or_insert((std::time::Instant::now(), 0));
                    e.0 = std::time::Instant::now();
                    e.1 += 1;
                }
                crate::transport::IncomingEvt::FileParsed {
                    path,
                    ast_hash,
                    definitions,
                } => {
                    self.file_parse_retry.remove(&path);
                    self.file_ast_hashes.insert(path.clone(), ast_hash);
                    // If a module row's payload.path matches, synthesize
                    // child TreeNodes from the parsed definitions and
                    // splice. Files-mode drift-detection callers ignore
                    // `definitions` (they just want ast_hash); modules-mode
                    // expansion callers consume it here. Same wire shape,
                    // both consumers happy.
                    let module_id = self
                        .tree
                        .rows
                        .iter()
                        .find(|r| {
                            r.node.kind == "module"
                                && r.node.payload.get("path").and_then(|v| v.as_str())
                                    == Some(path.as_str())
                        })
                        .map(|r| r.node.id.clone());
                    if let Some(parent_id) = module_id {
                        // Strip the `modules:` prefix to recover the module
                        // name for col-3's `function.methods` call later.
                        // The module's TreeNode lives at parent_id, so this
                        // is the same string the kernel knows it by.
                        let module_name = parent_id
                            .strip_prefix("modules:")
                            .unwrap_or(&parent_id)
                            .to_string();
                        let kids: Vec<TreeNode> = definitions
                            .into_iter()
                            .map(|d| {
                                // Function rows get has_children=true so
                                // Enter/Right fires `function.methods` for
                                // them. Module name rides on payload so the
                                // chrome doesn't have to re-parse the id.
                                // Non-function defs (struct, abstract, …)
                                // stay leaves for now.
                                let is_function = d.kind == "function";
                                let mut payload = serde_json::Map::new();
                                if is_function {
                                    payload.insert(
                                        "module".to_string(),
                                        serde_json::Value::String(module_name.clone()),
                                    );
                                    payload.insert(
                                        "name".to_string(),
                                        serde_json::Value::String(d.name.clone()),
                                    );
                                }
                                TreeNode {
                                    id: format!("{parent_id}:{}", d.name),
                                    label: format!("{} ({})", d.name, d.kind),
                                    kind: d.kind,
                                    has_children: is_function,
                                    badges: Vec::new(),
                                    payload,
                                }
                            })
                            .collect();
                        self.tree.apply_children(&parent_id, kids);
                    }
                }
                crate::transport::IncomingEvt::FunctionMethodsReceived {
                    module,
                    name,
                    methods,
                } => {
                    // Find the function row whose id matches `modules:<mod>:<name>`.
                    // The exact id is what we built when modules-col-2 splice
                    // ran, so reconstruct it from the request echo.
                    let parent_id = format!("modules:{module}:{name}");
                    let exists = self.tree.rows.iter().any(|r| r.node.id == parent_id);
                    if !exists {
                        tracing::debug!(
                            %parent_id,
                            "function.methods reply for unknown row — ignoring"
                        );
                        continue;
                    }
                    let kids: Vec<TreeNode> = methods
                        .into_iter()
                        .enumerate()
                        .map(|(i, m)| {
                            // `sig` is the standard `string(m)` repr, which
                            // ends in ` @ <module> <file>:<line>`. Trim that
                            // tail for the row label so the parameter
                            // signature reads cleanly; the location lives
                            // on payload for a future jump-to-line UX.
                            let label = m
                                .sig
                                .split_once(" @ ")
                                .map(|(head, _)| head.to_string())
                                .unwrap_or(m.sig.clone());
                            TreeNode {
                                id: format!("{parent_id}#{i}"),
                                label,
                                kind: "method".to_string(),
                                has_children: false,
                                badges: Vec::new(),
                                payload: Default::default(),
                            }
                        })
                        .collect();
                    self.tree.apply_children(&parent_id, kids);
                }
                crate::transport::IncomingEvt::ConceptRead {
                    target,
                    exists,
                    content,
                } => {
                    // Two consumers for concept.read replies:
                    //   1) Edit-mode stale-reload: when the user picks
                    //      `r` on the stale banner we re-fire the read
                    //      and replace the edit buffer with the on-disk
                    //      content. Matches by edit_state.target so it
                    //      doesn't collide with the cursor-tracking
                    //      read.
                    //   2) Cursor-tracking read: the usual path that
                    //      populates `concept` + `preview_concept` for
                    //      the read-only view.
                    let stale_reload = self
                        .edit_state
                        .as_ref()
                        .map(|e| e.stale_banner && e.target == target)
                        .unwrap_or(false);
                    if stale_reload {
                        if let Some(edit) = self.edit_state.as_mut() {
                            let (header, body) = split_frontmatter(&content);
                            edit.header = header;
                            edit.expected_ast_hash = if exists {
                                parse_synced_against(&content)
                            } else {
                                None
                            };
                            edit.original = body.clone();
                            edit.buf = EditBuffer::new(body);
                            edit.stale_banner = false;
                            edit.confirm_discard = false;
                        }
                        self.rebuild_edit_preview();
                        // Also let the cursor-tracking path update its
                        // cache so the read-only view shows fresh
                        // content if the user exits edit mode.
                    }
                    // Drop if the cursor has moved since we fired this read;
                    // the next `maybe_fire_concept_read` will issue a fresh
                    // request for the current selection.
                    if self.concept_target_fired.as_deref() == Some(target.as_str()) {
                        let synced_against = if exists {
                            parse_synced_against(&content)
                        } else {
                            None
                        };
                        if exists {
                            let body = strip_frontmatter(&content);
                            self.preview_concept = Some(MarkdownPreview::new(
                                self.text.font_system_mut(),
                                &body,
                                self.concept_rect_px.w.max(1.0),
                                self.concept_rect_px.h.max(1.0),
                                self.scale,
                                &MathMetricsMap::new(),
                                &FigureMetricsMap::new(),
                                &self.highlight_service,
                                &self.markdown_token_cache,
                            ));
                        } else {
                            self.preview_concept = None;
                        }
                        self.concept = Some(ConceptInfo {
                            target,
                            exists,
                            content,
                            synced_against,
                        });
                    }
                }
                crate::transport::IncomingEvt::Preview {
                    node_id,
                    workspace_id,
                    mime,
                    bytes,
                    extras,
                } => {
                    // Drop a preview.get reply for a workspace we've since
                    // switched away from. An in-flight cross-workspace reply
                    // would otherwise clobber the restored preview_src on a
                    // workspace round-trip (A→B→A: the nav cursor restores to
                    // A's file from A's snapshot, but B's late blob lands + paints
                    // over it — the round-trip preview/blob mismatch repro'd
                    // 2026-06-24). Mirrors the tree.root workspace-scoping above
                    // (gpu.rs ~6560); `workspace_id` is the ws the request was
                    // fired for (threaded back via PendingKind::PreviewGet,
                    // transport.rs:2677), `None` == the daemon-default workspace.
                    if workspace_id != self.active_workspace_id {
                        tracing::debug!(?workspace_id, active = ?self.active_workspace_id,
                            "drop stale cross-workspace preview.get reply");
                        continue;
                    }
                    // Cache the source so a runtime font-size change
                    // can re-render at the new scale without a
                    // round-trip to the backend.
                    self.preview_src = Some((mime.clone(), bytes.clone()));
                    // Pagination state (ADR 0021): present only when the
                    // serving plugin reported page extras; anything else
                    // (including a later unpaginated reply for a new
                    // cursor target) clears it, retiring the n/p keys.
                    self.preview_page = extras.as_ref().and_then(|e| {
                        let page = e.get("page")?.as_u64()? as u32;
                        let count = e.get("page_count")?.as_u64()? as u32;
                        Some((page, count))
                    });
                    // Is this the higher-res reply to a zoom re-raster of the
                    // page already on screen? Only if a re-raster is pending
                    // for the SAME page — a reply for a different page is a
                    // real navigation (page turn / cursor move) and resets the
                    // view to fit.
                    let is_reraster = matches!(
                        (self.preview_page_raster_pending, self.preview_page),
                        (Some((pend, _)), Some((cur, _))) if pend == cur
                    );
                    if is_reraster {
                        if let Some((_, z)) = self.preview_page_raster_pending.take() {
                            self.preview_page_raster_zoom = z;
                        }
                        self.preview_reraster_keep_view = true;
                    } else {
                        self.preview_page_raster_zoom = 1.0;
                        self.preview_page_raster_pending = None;
                        self.preview_reraster_keep_view = false;
                    }
                    // For markdown previews, also remember which node
                    // id + workspace served the buffer — figure URL
                    // resolution needs the markdown file's directory,
                    // and figure fetches must go to the same workspace
                    // (otherwise active_workspace_id drift sends the
                    // request to a project that doesn't have the file).
                    if matches!(mime.as_str(), "text/markdown" | "text/x-markdown") {
                        if let Some(id) = node_id.as_ref() {
                            self.current_md_node_id = Some(id.clone());
                            self.current_md_workspace_id = workspace_id;
                        }
                    }
                    self.render_preview_source(&mime, &bytes);
                    // Modules-mode line anchoring: render_preview_source just
                    // reset the scroll to the top; if the selected row gave us
                    // a definition line, scroll the item (its docstring if
                    // present, else the definition) to the top instead of
                    // showing the containing file from line 1. Consume-once,
                    // code shapers only (tokens / non-markdown text), and only
                    // for the reply matching the request we anchored.
                    if let Some(def_line) = self.preview_anchor_line.take() {
                        let is_code = mime.starts_with("application/vnd.sot.tokens+json")
                            || (mime.starts_with("text/")
                                && mime != "text/markdown"
                                && mime != "text/x-markdown");
                        let matches_req =
                            node_id.as_deref() == self.preview_node_id_fired.as_deref();
                        if def_line > 0 && is_code && matches_req {
                            self.preview_scroll = self
                                .preview_md
                                .anchor_scroll_for_def_line(def_line as usize);
                            self.preview_anchored_to = Some(def_line);
                        } else {
                            self.preview_anchored_to = None;
                        }
                    } else {
                        self.preview_anchored_to = None;
                    }
                }
                crate::transport::IncomingEvt::FigureLoaded { url, mime, bytes } => {
                    // Decode the bytes into a Quad sized to the
                    // bitmap's natural pixel dimensions, then drop it
                    // into figure_cache keyed by the original markdown
                    // URL. `needs_md_reflow` forces a one-shot walk
                    // before the next paint so the placeholder's
                    // reserved height tracks the figure's actual aspect
                    // — without it the FFFC stays at the
                    // FIGURE_BLOCK_H_DEFAULT fallback even after the
                    // bytes land.
                    self.figure_pending.remove(&url);
                    match decode_figure_bytes(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &mime,
                        &bytes,
                    ) {
                        Ok(entry) => {
                            tracing::info!(
                                %url,
                                %mime,
                                w = entry.natural_w_px,
                                h = entry.natural_h_px,
                                "figure decoded"
                            );
                            self.figure_cache.insert(url, entry);
                            self.needs_md_reflow = true;
                            self.window.request_redraw();
                        }
                        Err(e) => {
                            tracing::warn!(%url, %mime, error = %e, "figure decode failed");
                            // Terminal: collapse the reservation to the
                            // compact fallback on the next reflow rather
                            // than leaving an empty box that will never
                            // be painted over.
                            self.figure_failed.insert(url);
                            self.needs_md_reflow = true;
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::MathRendered {
                    latex,
                    svg_bytes,
                    ex,
                    display,
                } => {
                    // Stash in the (latex, display)-keyed cache for the
                    // A3 paint pass. Parse the SVG's ex-unit width/height
                    // up front so the rasterise step can size the pixmap
                    // relative to body font instead of fit-stretching the
                    // SVG into a fixed-pixel letterbox (which is what
                    // made every display block render ~4× oversized).
                    let (width_ex, height_ex, vertical_align_ex) = parse_math_svg_dims(&svg_bytes);
                    let key = (latex.clone(), display);
                    self.math_cache.insert(
                        key,
                        MathSvg {
                            svg_bytes: svg_bytes.clone(),
                            ex,
                            width_ex,
                            height_ex,
                            vertical_align_ex,
                            rasterised: None,
                        },
                    );
                    self.math_pending.remove(&(latex, display));
                    // Force a one-shot rebuild of preview_md before the
                    // next paint so the walk consults the freshly-cached
                    // dims when reserving each block's vertical space.
                    self.needs_md_reflow = true;
                    self.window.request_redraw();
                    // Also keep the old standalone math-pane preview
                    // path alive for the M1 acceptance test fixture
                    // (`requirements.md` has a canonical integral
                    // expectation against `preview_svg`). Soon the
                    // pane will be retired in favour of inline
                    // markdown placement.
                    match quad_from_svg_bytes(
                        &self.device,
                        &self.queue,
                        &self.quad_pipeline,
                        &svg_bytes,
                        1024,
                        256,
                    ) {
                        Ok(q) => {
                            tracing::info!(bytes = svg_bytes.len(), "math SVG rasterised");
                            self.preview_svg = Some(q);
                        }
                        Err(e) => tracing::warn!(error = %e, "math SVG rasterise failed"),
                    }
                }
                crate::transport::IncomingEvt::MarkdownTokens {
                    lang,
                    source_hash,
                    spans,
                } => {
                    // Backend semantic overlay landed. Stash in the per-fence
                    // cache, clear in-flight pending, and ask for a reflow so
                    // the next redraw consumes the cache instead of relying
                    // on the tree-sitter base alone.
                    let key = (lang.clone(), source_hash);
                    let n = spans.len();
                    self.markdown_token_cache.insert(key.clone(), spans);
                    self.markdown_token_pending.remove(&key);
                    tracing::debug!(
                        %lang,
                        source_hash,
                        spans = n,
                        "markdown.tokens received → cache + reflow"
                    );
                    self.needs_md_reflow = true;
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ReplEvalDone {
                    eval_id,
                    elapsed_ms,
                    frames,
                } => {
                    // ADR 0014 reply routing. Look up which workspace
                    // this eval was fired for; if it matches the active
                    // workspace, mutate the live `repl_log`; otherwise
                    // splice the result into the originating workspace's
                    // snapshot so the user sees the completed entry when
                    // they swap back. An eval with no recorded owner
                    // falls through to the live log (legacy / restart-
                    // gap behavior).
                    // ADR 0009 phase-2: empty-frames + 0-elapsed is an early
                    // *acceptance* ack (the eval was queued, not yet run). The
                    // streamed `Done` frame owns completion — it finalizes the
                    // entry and drops the routing key. So peek here instead of
                    // removing: removing now would orphan the key before the
                    // frames arrive, dropping a swapped-away eval's frames. Only
                    // a legacy synchronous-collect ack (real frames/elapsed)
                    // finalizes + removes inline.
                    let acceptance = frames.is_empty() && elapsed_ms == 0;
                    let owner = if acceptance {
                        self.eval_id_workspace.get(&eval_id).cloned()
                    } else {
                        self.eval_id_workspace.remove(&eval_id)
                    };
                    let active_key = self.current_workspace_key();
                    match owner.as_deref() {
                        Some(key) if key != active_key.as_str() => {
                            if let Some(snap) = self.workspace_repl_snapshots.get_mut(key) {
                                if let Some(entry) =
                                    snap.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                {
                                    if !acceptance {
                                        if !frames.is_empty() {
                                            entry.frames = frames;
                                        }
                                        entry.elapsed_ms = elapsed_ms;
                                        entry.in_flight = false;
                                    }
                                } else {
                                    tracing::debug!(
                                        eval_id,
                                        ?key,
                                        "repl.eval reply for unknown id in snapshot — ignoring"
                                    );
                                }
                            } else {
                                tracing::debug!(
                                    eval_id,
                                    ?key,
                                    "repl.eval reply for workspace with no snapshot — ignoring"
                                );
                            }
                        }
                        _ => {
                            if let Some(entry) =
                                self.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                            {
                                if !acceptance {
                                    if !frames.is_empty() {
                                        entry.frames = frames;
                                    }
                                    entry.elapsed_ms = elapsed_ms;
                                    entry.in_flight = false;
                                }
                            } else {
                                tracing::debug!(
                                    eval_id,
                                    "repl.eval reply for unknown id — ignoring"
                                );
                            }
                        }
                    }
                }
                crate::transport::IncomingEvt::MonitorSubscribed { hosts, .. } => {
                    self.monitor_view.set_roster(hosts);
                    self.monitor_dirty = true;
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::MonitorHistory { hosts } => {
                    self.monitor_view.apply_history(hosts);
                    self.monitor_dirty = true;
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::MonitorTick { hosts } => {
                    for h in hosts {
                        self.monitor_view.apply_tick(h);
                    }
                    self.monitor_dirty = true;
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ReplFrameStreamed {
                    eval_id,
                    workspace_id,
                    frame,
                } => {
                    // ADR 0009 phase-2 live streaming: append each frame to the
                    // in-flight `repl_log` entry as it arrives (vs the old
                    // synchronous-collect on ReplEvalDone). Routing mirrors
                    // ReplEvalDone — the entry may be in the active log or, if
                    // its workspace was swapped away, that workspace's snapshot.
                    // We key on the recorded eval_id->workspace map (kept until
                    // the terminal ack drops it); `workspace_id` is a hint.
                    // `Done` finalizes (in_flight=false + elapsed); others append.
                    let _ = workspace_id;
                    // Capture the terminal-frame flag before `frame` is moved into
                    // the match below — on Done we run the terminal cleanup the
                    // acceptance ack intentionally deferred to us.
                    let done_elapsed = if let ReplFrame::Done { elapsed_ms, .. } = &frame {
                        Some(*elapsed_ms)
                    } else {
                        None
                    };
                    let owner = self.eval_id_workspace.get(&eval_id).cloned();
                    let active_key = self.current_workspace_key();
                    let entry: Option<&mut ReplEntry> = match owner.as_deref() {
                        Some(key) if key != active_key.as_str() => {
                            self.workspace_repl_snapshots.get_mut(key).and_then(|snap| {
                                snap.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                            })
                        }
                        _ => self.repl_log.iter_mut().find(|e| e.eval_id == eval_id),
                    };
                    if let Some(entry) = entry {
                        match frame {
                            ReplFrame::Done { elapsed_ms, .. } => {
                                tracing::debug!(eval_id, elapsed_ms, "repl.frame: done (finalize)");
                                entry.elapsed_ms = elapsed_ms;
                                entry.in_flight = false;
                            }
                            other => {
                                // debug, not info — one line per streamed frame
                                // is too noisy for the default log. Raise to
                                // RUST_LOG=debug to watch live-append timing.
                                tracing::debug!(eval_id, frame = ?other, "repl.frame: append");
                                entry.frames.push(other);
                            }
                        }
                    } else {
                        tracing::warn!(
                            eval_id,
                            "repl.frame dropped: no in-flight entry for eval_id"
                        );
                    }
                    if let Some(done_elapsed) = done_elapsed {
                        // Terminal frame: the acceptance ack deliberately left the
                        // routing key (and, for run_file, the status) for us. Drop
                        // the key and finalize the run_file status with the real
                        // elapsed (the ack's was a 0 placeholder, sent pre-run).
                        self.eval_id_workspace.remove(&eval_id);
                        if let Some((basename, project_dir, fresh)) =
                            self.repl_runfile_status.remove(&eval_id)
                        {
                            self.status = if fresh {
                                let proj = project_dir.as_deref().unwrap_or("(no project)");
                                format!(
                                    "ran '{basename}' (fresh — project: {proj}, {done_elapsed}ms)"
                                )
                            } else {
                                format!("ran '{basename}' (existing repl, {done_elapsed}ms)")
                            };
                        }
                    }
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ConceptWriteDone { target, result } => {
                    // Only reconcile when the reply targets the active
                    // edit — late replies for an abandoned edit are
                    // ignored. Stale-write banner UI lands in a later
                    // commit; for v1 we log loudly and trust the backend's
                    // refusal (no auto-clobber, no silent overwrite).
                    let matches_active = self
                        .edit_state
                        .as_ref()
                        .map(|e| e.target == target)
                        .unwrap_or(false);
                    match result {
                        crate::transport::ConceptWriteResult::Ok { path, written } => {
                            tracing::info!(%target, %path, written, "concept.write ok");
                            if matches_active {
                                // Snap `original` so dirty-check matches
                                // the new on-disk state — the user can
                                // keep editing without an instant dirty
                                // flag after a save.
                                if let Some(edit) = self.edit_state.as_mut() {
                                    edit.original = edit.buf.body().to_string();
                                }
                            }
                        }
                        crate::transport::ConceptWriteResult::Stale => {
                            tracing::warn!(%target, "concept.write refused: stale");
                            if matches_active {
                                if let Some(edit) = self.edit_state.as_mut() {
                                    edit.stale_banner = true;
                                }
                                self.rebuild_edit_preview();
                            }
                        }
                        crate::transport::ConceptWriteResult::Error { code, message } => {
                            tracing::error!(%target, %code, %message, "concept.write failed");
                        }
                    }
                }
                crate::transport::IncomingEvt::FileRead {
                    node_id,
                    exists,
                    content,
                    version,
                } => {
                    // Edit-enter (or stale-reload) for a general file: when this
                    // reply matches the pending request and the file exists, open
                    // the editor on it (replacing any prior edit_state — that's
                    // how `r` reload-discards). Non-pending replies are ignored.
                    if self.pending_file_edit.as_deref() == Some(node_id.as_str()) {
                        self.pending_file_edit = None;
                        if exists {
                            self.edit_state = Some(EditState {
                                target: node_id.clone(),
                                expected_ast_hash: None,
                                header: None,
                                original: content.clone(),
                                buf: EditBuffer::new(content),
                                confirm_discard: false,
                                stale_banner: false,
                                file_node_id: Some(node_id.clone()),
                                file_version: Some(version),
                            });
                            self.rebuild_edit_preview();
                            tracing::info!(%node_id, "entered file edit mode");
                        } else {
                            tracing::warn!(%node_id, "file.read: not found — not entering edit");
                        }
                    } else {
                        tracing::debug!(%node_id, exists, "file.read reply (no pending edit)");
                    }
                }
                crate::transport::IncomingEvt::FileWriteDone { node_id, result } => {
                    // Reconcile only when the reply targets the active file edit
                    // (late replies for an abandoned edit are ignored).
                    let matches_active = self
                        .edit_state
                        .as_ref()
                        .and_then(|e| e.file_node_id.as_deref())
                        == Some(node_id.as_str());
                    // Ctrl+N new-file round-trip: did this reply close out the
                    // file we just asked the backend to create? If so, refresh
                    // the new file's parent dir so the row shows up.
                    let matches_create =
                        self.pending_created_node_id.as_deref() == Some(node_id.as_str());
                    match result {
                        crate::transport::FileWriteResult::Ok { path, version } => {
                            tracing::info!(%node_id, %path, %version, "file.write ok");
                            if matches_active {
                                if let Some(edit) = self.edit_state.as_mut() {
                                    // Snap the dirty baseline + adopt the new
                                    // version so further edits start clean and
                                    // the next save's conflict check is current.
                                    edit.original = edit.buf.body().to_string();
                                    edit.file_version = Some(version);
                                }
                            }
                            if matches_create {
                                self.pending_created_node_id = None;
                                // Re-list the parent dir so the new file
                                // appears without a manual re-expand — same
                                // tree.children refresh the upload path uses.
                                let parent = parent_files_node_id(&node_id);
                                if let Err(e) =
                                    self.req_tx
                                        .send(crate::transport::OutgoingReq::TreeChildren {
                                            parent_id: parent,
                                            workspace_id: self.active_workspace_id.clone(),
                                        })
                                {
                                    tracing::warn!(error = %e,
                                        "drop post-create tree.children refresh");
                                }
                                let name = node_id
                                    .rsplit(['/', ':'])
                                    .next()
                                    .unwrap_or(node_id.as_str());
                                self.status = format!("created · {name}");
                                self.window.request_redraw();
                            }
                        }
                        crate::transport::FileWriteResult::Conflict {
                            current_version, ..
                        } => {
                            tracing::warn!(%node_id, %current_version, "file.write refused: conflict");
                            if matches_active {
                                if let Some(edit) = self.edit_state.as_mut() {
                                    edit.stale_banner = true;
                                }
                                self.rebuild_edit_preview();
                            }
                            if matches_create {
                                // The name collided on disk (a file the tree
                                // didn't list yet). Drop the pending id and
                                // surface it.
                                self.pending_created_node_id = None;
                                self.status = "new file · already exists on disk".to_string();
                                self.window.request_redraw();
                            }
                        }
                        crate::transport::FileWriteResult::Error { code, message } => {
                            tracing::error!(%node_id, %code, %message, "file.write failed");
                            if matches_create {
                                self.pending_created_node_id = None;
                                self.status = format!("new file failed · {message}");
                                self.window.request_redraw();
                            }
                        }
                    }
                }
                crate::transport::IncomingEvt::FileDeleteDone { node_id, result } => {
                    // Ctrl+D delete round-trip: did this reply close out the
                    // file we just asked the backend to trash? Late replies for
                    // a stale request are ignored.
                    let matches_delete =
                        self.pending_deleted_node_id.as_deref() == Some(node_id.as_str());
                    match result {
                        crate::transport::FileDeleteResult::Ok {
                            path,
                            trashed,
                            trash_path,
                        } => {
                            tracing::info!(%node_id, %path, trashed, ?trash_path, "file.delete ok");
                            if matches_delete {
                                self.pending_deleted_node_id = None;
                                // Re-list the parent dir so the deleted row
                                // vanishes without a manual re-expand — same
                                // tree.children refresh the create path uses.
                                // TreeView reconciliation re-clamps the cursor.
                                let parent = parent_files_node_id(&node_id);
                                if let Err(e) =
                                    self.req_tx
                                        .send(crate::transport::OutgoingReq::TreeChildren {
                                            parent_id: parent,
                                            workspace_id: self.active_workspace_id.clone(),
                                        })
                                {
                                    tracing::warn!(error = %e,
                                        "drop post-delete tree.children refresh");
                                }
                                let name = node_id
                                    .rsplit(['/', ':'])
                                    .next()
                                    .unwrap_or(node_id.as_str());
                                self.status = match trash_path {
                                    Some(tp) => format!("deleted · {name} → {tp}"),
                                    None => format!("deleted · {name}"),
                                };
                                self.window.request_redraw();
                            }
                        }
                        crate::transport::FileDeleteResult::Error { code, message } => {
                            tracing::error!(%node_id, %code, %message, "file.delete failed");
                            if matches_delete {
                                self.pending_deleted_node_id = None;
                                self.status = format!("delete failed · {code}: {message}");
                                self.window.request_redraw();
                            }
                        }
                    }
                }
                crate::transport::IncomingEvt::PtyOpened {
                    cols,
                    rows,
                    pane_command,
                } => {
                    // Backend confirmed the pty size. Make sure our
                    // emulator matches — if it doesn't (e.g. backend
                    // clamped to a minimum), the redraw will fire a
                    // PtyResize on the next mismatch.
                    self.pty_terminal.screen_mut().set_size(rows, cols);
                    self.pty_size = Some((cols, rows));
                    // Contract (b): the pty re-target is now live. If this
                    // open was for a flagged agent workspace, launch claude
                    // + deliver its bootstrap now that the BL pane points at
                    // the agent's session.
                    if let Some(sess) = self.pending_autostart.take() {
                        if self.bl_pane_target.as_deref() == Some(sess.as_str()) {
                            // Authoritative backend guard first: if the agent
                            // pane already runs claude (tmux foreground process
                            // is `claude`/`node`, reported on this `pty.open`),
                            // it's confirmed up — record it and skip the launch
                            // outright. This is the truth source the old path
                            // lacked: it survives FE relaunches (which wipe
                            // `autostarted_sessions`) and needs no screen-scrape
                            // fresh-output heuristic, so an idle long-lived
                            // agent is never
                            // relaunched into. Only when the signal is ambiguous
                            // (shell prompt, claude shelled out mid-tool, or the
                            // backend didn't probe) do we fall back to the
                            // settle-and-sniff scan below.
                            if pane_command_is_claude(pane_command.as_deref()) {
                                self.autostarted_sessions.insert(sess.clone());
                                self.launching_sessions.remove(&sess);
                                tracing::info!(session = %sess, ?pane_command,
                                    "autostart: backend reports claude already foreground in pane — skipping launch (no prompt-spam)");
                                self.status = format!(
                                    "auto-start: claude already running in {sess} — left as-is"
                                );
                            } else {
                                // Don't launch yet: arm the pre-launch sniff so
                                // we can still skip a pane that shows claude in
                                // its replayed screen even if the foreground
                                // probe was inconclusive.
                                // `advance_autostart_scan` decides once the
                                // replayed screen settles.
                                let now = std::time::Instant::now();
                                self.autostart_scan = Some(AutostartScan {
                                    session: sess,
                                    started: now,
                                    last_pty: now,
                                });
                            }
                        }
                    }
                    // Resume a delivery parked by a mid-flight switch-away,
                    // now that the re-target for its pinned session has
                    // landed (writes route to the right pane from here on).
                    // Fresh clocks restart the settle + 60s-timeout windows;
                    // phase is preserved — see `deferred_delivery`'s doc for
                    // why resuming at `Typed` must not re-type the task.
                    // (Mutually exclusive with the launch path above: a
                    // delivery only exists after `autostart_claude_in_pane`
                    // put the session in `autostarted_sessions`.)
                    if self.delivery.is_none()
                        && self.deferred_delivery.as_ref().is_some_and(|d| {
                            self.bl_pane_target.as_deref() == Some(d.pinned.as_str())
                        })
                    {
                        let mut d = self.deferred_delivery.take().expect("checked above");
                        let now = std::time::Instant::now();
                        d.started = now;
                        d.last_pty = now;
                        tracing::info!(pinned = %d.pinned, phase = ?d.phase,
                            "autostart: resuming deferred task delivery");
                        self.status = format!("auto-start: resuming task delivery to @{}", d.agent);
                        self.delivery = Some(d);
                    }
                }
                crate::transport::IncomingEvt::PtyBytes { bytes } => {
                    self.pty_terminal.process(&bytes);
                    // Feed the auto-start delivery settle-clock: output on the
                    // pinned pane means claude is still rendering (not ready
                    // yet), so reset its quiescence timer.
                    let on_pinned = match self.delivery.as_ref() {
                        Some(d) => self.bl_pane_target.as_deref() == Some(d.pinned.as_str()),
                        None => false,
                    };
                    if on_pinned {
                        if let Some(d) = self.delivery.as_mut() {
                            d.last_pty = std::time::Instant::now();
                        }
                    }
                    // Same settle-clock for the pre-launch sniff: output on the
                    // scanned pane means tmux is still replaying its screen.
                    let scan_on_pinned = match self.autostart_scan.as_ref() {
                        Some(s) => self.bl_pane_target.as_deref() == Some(s.session.as_str()),
                        None => false,
                    };
                    if scan_on_pinned {
                        if let Some(s) = self.autostart_scan.as_mut() {
                            s.last_pty = std::time::Instant::now();
                        }
                    }
                    // Belt-and-braces wake of the redraw pump. The
                    // transport-side `window.request_redraw()` at
                    // transport.rs:1044 fires per frame received, which
                    // *should* already cover this — but user-confirmed
                    // 2026-05-22 the LLM pane sometimes stops repainting
                    // after a nav-reset burst (set_root / set_flat /
                    // apply_children + the cascade of `preview.get` /
                    // `concept.read` / `file.parse` / `tree.children`
                    // that follows). Switching workspaces b/f unwedges
                    // it because that path fires its own request_redraw.
                    // Independent of the underlying root cause, asking
                    // for a redraw here too guarantees the LLM pane
                    // wakes the loop on every byte burst — coalesced
                    // with the transport call by winit, so no extra
                    // paints in the steady state. See `a2e4916` for the
                    // companion fix that removes the *trigger* (nav
                    // cursor reset).
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::Event { op, payload } => {
                    if op == sot_protocol::op::WORKSPACE_CHANGED {
                        // Server pushed a workspace create/destroy; re-list so
                        // the Sessions strip refreshes live (mirror the manual
                        // poll). Idempotent if we triggered the change.
                        let _ = self
                            .req_tx
                            .send(crate::transport::OutgoingReq::WorkspaceList);
                    } else if op == sot_protocol::op::AGENT_MESSAGE {
                        // Server relayed an agent-to-agent message over the
                        // SSH-forwarded socket. Item 2: a session can drive
                        // this FE's nav by broadcasting a `sot_ui` envelope
                        // as the message text — intercept it BEFORE the inbox
                        // append. A nav command is acted on (when it targets
                        // our active workspace) and NEVER filed as chat;
                        // anything else is an ordinary message → append to
                        // fe-inbox.jsonl so the in-terminal agent on this
                        // machine receives it instantly (mirror the
                        // workspace.changed push leg).
                        let text = payload.get("text").and_then(|v| v.as_str()).unwrap_or("");
                        if let Some(env) = parse_nav_envelope(text) {
                            self.handle_nav_envelope(&env);
                        } else {
                            append_agent_message(&payload);
                        }
                    } else if op == sot_protocol::op::FE_COMMAND {
                        // ADR 0025 imperative FE command. The daemon broadcasts
                        // to every connection (like agent.message); we parse,
                        // self-filter on `target`, and route to an `FeCommand`
                        // run through the existing `dispatch_fe_command` sink.
                        match serde_json::from_value::<sot_protocol::ops::FeCommandEvt>(payload) {
                            Ok(evt) => {
                                // route_fe_command applies the target filter
                                // (None = all FEs act; Some(self) = act,
                                // force-show eligible; Some(other) = ignore) and
                                // maps cmd→FeCommand (None = bad target / unknown
                                // cmd / missing arg). `urgent` rides on the
                                // mapped Preview/Reveal; the idle gate is applied
                                // in dispatch_fe_command, not here.
                                if let Some(cmd) = route_fe_command(&evt, &self_comm_handle()) {
                                    tracing::info!(cmd = %evt.cmd, target = ?evt.target,
                                        "fe.command: dispatching");
                                    self.dispatch_fe_command(cmd);
                                } else {
                                    tracing::debug!(cmd = %evt.cmd, target = ?evt.target,
                                        "fe.command: ignored (target mismatch / unknown cmd / missing arg)");
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "fe.command: malformed payload — ignoring");
                            }
                        }
                    } else if op == sot_protocol::op::PREVIEW_CHANGED {
                        // The daemon's file watcher reported a filesystem change
                        // (create / modify / remove). On a create or remove the
                        // affected directory's listing changed, so live-refresh
                        // it in the Files nav tree — otherwise the pane shows a
                        // stale listing until a manual re-nav (the reported bug).
                        //
                        // 2026-07-10 multiwatch: the daemon now runs a watcher
                        // per WORKSPACE and tags each event with the owning
                        // slug. Only act on events for the workspace this FE is
                        // viewing — node ids are workspace-relative
                        // (`files:README.md` exists in every repo), so acting on
                        // a foreign workspace's event would refresh or re-fetch
                        // the WRONG file. A missing tag (pre-multiwatch daemon)
                        // passes through, preserving the old behavior.
                        let evt_ws = payload.get("workspace_id").and_then(|v| v.as_str());
                        if let Some(slug) = evt_ws {
                            if !preview_targets_active_ws(
                                self.active_workspace_id.as_deref(),
                                self.default_workspace_slug.as_deref(),
                                slug,
                            ) {
                                continue;
                            }
                        }
                        let kind = payload.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                        let changed_node_id = payload.get("node_id").and_then(|v| v.as_str());
                        if kind == "created" || kind == "removed" {
                            if let Some(node_id) = changed_node_id {
                                let parent = parent_files_node_id(node_id);
                                self.refresh_tree_dir_if_expanded(&parent);
                            }
                        } else if kind == "modified" {
                            // A modify leaves the directory *listing* unchanged,
                            // but if the modified file is the one the preview pane
                            // is currently showing, its bytes changed underneath
                            // us (e.g. a tool re-rendered a PDF in place). Re-fire
                            // `preview.get` so the pane reflects the new content
                            // instead of the stale render. `preview_node_id_fired`
                            // is the source of truth for "what the preview pane is
                            // showing right now" (same anchor the reconnect
                            // re-fetch uses); hold the current page so a paginated
                            // preview doesn't snap back to page 1.
                            if let Some(node_id) = changed_node_id {
                                if self.preview_node_id_fired.as_deref() == Some(node_id) {
                                    let (fit_w, fit_h) = self.preview_fit_px();
                                    let _ = self.req_tx.send(
                                        crate::transport::OutgoingReq::PreviewGet {
                                            node_id: node_id.to_string(),
                                            workspace_id: self.active_workspace_id.clone(),
                                            page: self.preview_page.map(|(p, _)| p),
                                            fit_w,
                                            fit_h,
                                        },
                                    );
                                }
                            }
                        }
                    } else {
                        tracing::debug!(%op, "evt");
                    }
                }
                // Sessions-mode events (ADR 0013). Synthesize TreeNodes
                // for sessions / panes so Sessions mode reuses the same
                // TreeView rendering Files / Modules use; capture-pane
                // text feeds through `render_preview_source` as plain text.
                crate::transport::IncomingEvt::TmuxSessions { sessions } => {
                    let root = TreeNode {
                        id: "sessions:".to_string(),
                        label: "sessions".to_string(),
                        kind: "sessions".to_string(),
                        // Always show the [+ create new] row even when the
                        // sessions list is empty (fresh host).
                        has_children: true,
                        badges: Vec::new(),
                        payload: Default::default(),
                    };
                    let create_row = TreeNode {
                        id: "sessions:+create".to_string(),
                        label: "[+ create new]".to_string(),
                        kind: "session_create".to_string(),
                        has_children: false,
                        badges: Vec::new(),
                        payload: Default::default(),
                    };
                    let mut children = vec![create_row];
                    // Only surface tmux sessions that belong to Ship of Tools. Other
                    // sessions on the host (the user's own work, system
                    // sessions, etc.) are intentionally hidden — Sessions mode
                    // is a sot workspace picker, not a tmux browser.
                    // Per ADR 0014, workspace tmux sessions are named
                    // `sot-be-<slug>`. Until the rename to "Workspaces"
                    // lands we still call this Sessions mode, but the visible
                    // list is workspace-scoped.
                    children.extend(
                        sessions
                            .into_iter()
                            .filter(|s| s.name.starts_with("sot-be-"))
                            .map(|s| {
                                let mut payload = serde_json::Map::new();
                                payload.insert("created".to_string(), serde_json::json!(s.created));
                                payload
                                    .insert("attached".to_string(), serde_json::json!(s.attached));
                                payload.insert("windows".to_string(), serde_json::json!(s.windows));
                                payload.insert("width".to_string(), serde_json::json!(s.width));
                                payload.insert("height".to_string(), serde_json::json!(s.height));
                                payload.insert(
                                    "name".to_string(),
                                    serde_json::Value::String(s.name.clone()),
                                );
                                TreeNode {
                                    id: format!("sessions:{}", s.name),
                                    label: s.name,
                                    kind: "session".to_string(),
                                    has_children: true,
                                    badges: Vec::new(),
                                    payload,
                                }
                            }),
                    );
                    self.tree.set_root(root, children);
                    if matches!(self.mode, Mode::Sessions) {
                        if let Some(n) = self.pending_initial_selection.take() {
                            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                        }
                    }
                }
                crate::transport::IncomingEvt::TmuxPanes { session, panes } => {
                    let Some(session) = session else {
                        // Server-wide list isn't consumed by the UI today;
                        // log and ignore.
                        tracing::debug!(count = panes.len(), "tmux.panes (server-wide)");
                        continue;
                    };
                    let parent_id = format!("sessions:{session}");
                    let children: Vec<TreeNode> = panes
                        .into_iter()
                        .map(|p| {
                            let mut payload = serde_json::Map::new();
                            payload.insert(
                                "session".to_string(),
                                serde_json::Value::String(p.session.clone()),
                            );
                            payload.insert(
                                "window_index".to_string(),
                                serde_json::json!(p.window_index),
                            );
                            payload
                                .insert("pane_index".to_string(), serde_json::json!(p.pane_index));
                            payload.insert(
                                "command".to_string(),
                                serde_json::Value::String(p.command.clone()),
                            );
                            payload.insert("pid".to_string(), serde_json::json!(p.pid));
                            payload.insert("width".to_string(), serde_json::json!(p.width));
                            payload.insert("height".to_string(), serde_json::json!(p.height));
                            payload.insert("active".to_string(), serde_json::json!(p.active));
                            payload.insert(
                                "tmux_pane_id".to_string(),
                                serde_json::Value::String(p.id.clone()),
                            );
                            let label = if p.title.is_empty() {
                                format!("{} · {}", p.id, p.command)
                            } else {
                                format!("{} · {} ({})", p.id, p.title, p.command)
                            };
                            TreeNode {
                                id: format!("sessions:{session}/{}", p.id),
                                label,
                                kind: "pane".to_string(),
                                has_children: false,
                                badges: Vec::new(),
                                payload,
                            }
                        })
                        .collect();
                    self.tree.apply_children(&parent_id, children);
                }
                crate::transport::IncomingEvt::TmuxSessionCreated { result } => {
                    match result {
                        Ok(name) => {
                            self.status = format!("session created · {name}");
                            // Refresh the workspace list so the new row appears.
                            if matches!(self.mode, Mode::Sessions) {
                                let _ = self.req_tx.send(OutgoingReq::WorkspaceList);
                            }
                        }
                        Err(msg) => {
                            self.status = format!("session create failed · {msg}");
                            tracing::warn!(error = %msg, "tmux.create_session failed");
                        }
                    }
                }
                crate::transport::IncomingEvt::TmuxSessionKilled { result } => match result {
                    Ok(name) => {
                        self.status = format!("session killed · {name}");
                        if matches!(self.mode, Mode::Sessions) {
                            let _ = self.req_tx.send(OutgoingReq::WorkspaceList);
                        }
                    }
                    Err(msg) => {
                        self.status = format!("session kill failed · {msg}");
                        tracing::warn!(error = %msg, "tmux.kill_session failed");
                    }
                },
                crate::transport::IncomingEvt::TmuxPaneCaptured { target, text } => {
                    // Route through render_preview_source as plain text so
                    // the existing markdown-plain renderer shapes it into
                    // the preview pane the same as a text file would.
                    // Drop the result if the cursor has moved off the
                    // target since the request fired — avoids racing
                    // captures painting stale panes.
                    let still_current =
                        self.tree.rows.get(self.tree.selected).map_or(false, |row| {
                            row.node
                                .payload
                                .get("tmux_pane_id")
                                .and_then(|v| v.as_str())
                                == Some(target.as_str())
                        });
                    if still_current {
                        self.render_preview_source("text/plain", text.as_bytes());
                    } else {
                        tracing::debug!(%target, "drop stale tmux.capture_pane reply");
                    }
                }
                crate::transport::IncomingEvt::DirectoryList { path, entries } => {
                    // Only consume if it matches the picker we have open
                    // — late replies for a previously-drilled directory
                    // would otherwise overwrite the new entries.
                    if let Some(p) = self.workspace_picker.as_mut() {
                        if p.current_path == path {
                            p.entries = entries;
                            if p.selected >= p.entries.len() {
                                p.selected = 0;
                            }
                            self.window.request_redraw();
                        } else {
                            tracing::debug!(%path, current = %p.current_path, "drop stale directory.list reply");
                        }
                    }
                }
                crate::transport::IncomingEvt::WorkspaceCreated { result } => {
                    match result {
                        Ok(info) => {
                            self.workspace_picker = None;
                            self.status = format!(
                                "workspace created · '{}' @ {}",
                                info.label, info.project_root
                            );
                            // The autostart cache is fed by workspace.list,
                            // which hasn't refreshed for this brand-new
                            // workspace yet — seed it so the switch below
                            // arms the ccb autostart on first attach
                            // (matches the autostart_claude: true the
                            // create request carried).
                            self.workspace_autostart.insert(
                                info.tmux_session.clone(),
                                WsAutostart {
                                    autostart_claude: true,
                                    agent_name: String::new(),
                                    task: String::new(),
                                },
                            );
                            self.switch_to_workspace(
                                Some(info.slug.clone()),
                                Some(info.tmux_session.clone()),
                            );
                        }
                        Err(msg) => {
                            self.status = format!("workspace.create failed · {msg}");
                            tracing::warn!(error = %msg, "workspace.create failed");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::WorkspaceDestroyed { result } => {
                    match result {
                        Ok(info) => {
                            // If the active workspace was the one we
                            // just destroyed, bounce to default. The
                            // backend already refused to destroy the
                            // default, so resetting active to None is
                            // always a valid target.
                            if self
                                .active_workspace_id
                                .as_deref()
                                .map(|s| s == info.slug || s == info.workspace_id)
                                .unwrap_or(false)
                            {
                                self.switch_to_workspace(None, None);
                            }
                            // Clean up per-workspace snapshot maps so a
                            // recreated workspace with the same slug
                            // doesn't inherit stale UI/REPL state.
                            self.workspace_ui_snapshots.remove(&info.slug);
                            self.workspace_repl_snapshots.remove(&info.slug);
                            self.workspace_labels.remove(&info.slug);
                            self.workspace_project_roots.remove(&info.slug);
                            let tmux_note = if info.tmux_killed {
                                ""
                            } else {
                                " (tmux already gone)"
                            };
                            let toml_note = if info.toml_removed {
                                ""
                            } else {
                                " (toml remove failed)"
                            };
                            self.status = format!(
                                "workspace destroyed · '{}'{}{}",
                                info.label, tmux_note, toml_note
                            );
                            // Refresh the Sessions tree so the row drops.
                            if let Err(e) = self
                                .req_tx
                                .send(crate::transport::OutgoingReq::WorkspaceList)
                            {
                                tracing::warn!(error = %e, "drop workspace.list after destroy");
                            }
                        }
                        Err(msg) => {
                            self.status = format!("workspace.destroy failed · {msg}");
                            tracing::warn!(error = %msg, "workspace.destroy failed");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::PlutoOpened { result } => match result {
                    Ok(url) => {
                        if let Err(e) = open_url_in_browser(&url) {
                            tracing::warn!(error = %e, %url,
                                    "pluto: open_url_in_browser failed");
                            self.status = format!("pluto.open browser-launch failed · {e}");
                        } else {
                            self.status = format!("pluto · opened {url}");
                        }
                        self.window.request_redraw();
                    }
                    Err(msg) => {
                        tracing::warn!(error = %msg, "pluto.open failed");
                        self.status = format!("pluto.open failed · {msg}");
                        self.window.request_redraw();
                    }
                },
                crate::transport::IncomingEvt::DocsOpened { result } => match result {
                    Ok(url) => {
                        if let Err(e) = open_url_in_browser(&url) {
                            tracing::warn!(error = %e, %url,
                                    "docs: open_url_in_browser failed");
                            self.status = format!("docs.open browser-launch failed · {e}");
                        } else {
                            self.status = format!("docs · opened {url}");
                        }
                        self.window.request_redraw();
                    }
                    Err(msg) => {
                        tracing::warn!(error = %msg, "docs.open failed");
                        self.status = format!("docs.open failed · {msg}");
                        self.window.request_redraw();
                    }
                },
                crate::transport::IncomingEvt::VideoOpened { result } => match result {
                    Ok(url) => {
                        if let Err(e) = open_url_in_browser(&url) {
                            tracing::warn!(error = %e, %url,
                                    "video: open_url_in_browser failed");
                            self.status = format!("video.open browser-launch failed · {e}");
                        } else {
                            self.status = "video · opened in browser".to_string();
                        }
                        self.window.request_redraw();
                    }
                    Err(msg) => {
                        tracing::warn!(error = %msg, "video.open failed");
                        self.status = format!("video.open failed · {msg}");
                        self.window.request_redraw();
                    }
                },
                crate::transport::IncomingEvt::QuartoOpened { result } => {
                    match result {
                        Ok(html) => {
                            // Backend rendered a self-contained HTML; write a
                            // temp file + OS-open it (same path as a text/html
                            // preview's `o`).
                            if let Err(e) = open_html_in_browser(&html) {
                                tracing::warn!(error = %e, "quarto: open_html_in_browser failed");
                                self.status = format!("quarto.open browser-launch failed · {e}");
                            } else {
                                self.status = "quarto · opened in browser".to_string();
                            }
                            self.window.request_redraw();
                        }
                        Err(msg) => {
                            tracing::warn!(error = %msg, "quarto.open failed");
                            self.status = format!("quarto.open failed · {msg}");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::FileDownloadProgress {
                    dest,
                    written,
                    total,
                    eof,
                } => {
                    let name = dest
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| dest.display().to_string());
                    if eof {
                        self.status = format!("downloaded · {name} ({written} bytes)");
                    } else {
                        self.status = format!("download · {name} {written}/{total}");
                    }
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::FileUploadAck {
                    offset: _,
                    done,
                    final_name,
                } => {
                    if done {
                        let (name, dir, dir_node_id) = match self.upload.take() {
                            Some(up) => (final_name.unwrap_or(up.name), up.dir, up.dir_node_id),
                            None => (final_name.unwrap_or_default(), String::new(), String::new()),
                        };
                        self.status = format!("uploaded · {name} → {dir}");
                        // Refresh the destination dir's listing so the new file
                        // shows up without a manual re-expand.
                        if !dir_node_id.is_empty() {
                            if let Err(e) =
                                self.req_tx
                                    .send(crate::transport::OutgoingReq::TreeChildren {
                                        parent_id: dir_node_id,
                                        workspace_id: self.active_workspace_id.clone(),
                                    })
                            {
                                tracing::warn!(error = %e, "drop post-upload tree.children refresh");
                            }
                        }
                        self.window.request_redraw();
                    } else {
                        // The chunk-0 ack returns the backend's resolved name
                        // (sanitized + de-duped, e.g. `report (1).csv`). Adopt it
                        // so chunks 1..N target that same file.
                        if let (Some(fname), Some(up)) = (final_name, self.upload.as_mut()) {
                            up.name = fname;
                        }
                        // Flow control: ack of chunk N → send chunk N+1.
                        self.send_next_upload_chunk();
                    }
                }
                crate::transport::IncomingEvt::FileTransferFailed { op, message } => {
                    if op == "upload" {
                        self.upload = None;
                    }
                    self.status = format!("{op} failed · {message}");
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ImageCropped {
                    node_id,
                    path,
                    x,
                    y,
                    w,
                    h,
                    src_w,
                    src_h,
                } => {
                    // ADR 0022: paste a ready-to-send "look at this" line into
                    // the LLM pane (BL pty). No trailing Enter — the user can
                    // add context and submit, so we never fire a half-formed
                    // prompt or clobber partial input in a shared pane. The
                    // message names the *full source path* (provenance) and the
                    // crop path; Claude Code auto-attaches the crop image from
                    // its path, so the in-pane agent sees the actual pixels.
                    let name = node_id
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&node_id)
                        .to_string();
                    let src_path = self.backend_abs_path(&node_id);
                    let msg = format!(
                        "Look at this cropped region of {src_path} ({src_w}×{src_h} source) — \
                         ROI x={x} y={y}, {w}×{h} px. Cropped PNG: {path}"
                    );
                    let bytes = bracketed_paste_bytes(&msg);
                    if self
                        .req_tx
                        .send(crate::transport::OutgoingReq::PtyWrite { bytes })
                        .is_ok()
                    {
                        self.status =
                            format!("ROI {w}×{h} of {name} → LLM pane · Enter there to send");
                    } else {
                        self.status = "capture: paste to LLM failed (transport closed)".to_string();
                    }
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ImageCropFailed { node_id, message } => {
                    let name = node_id
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&node_id)
                        .to_string();
                    self.status = format!("capture failed · {name}: {message}");
                    self.window.request_redraw();
                }
                crate::transport::IncomingEvt::ReplRunFileDone { eval_id, result } => {
                    // J5: route frames into the pre-registered `repl_log`
                    // entry so the drawer scrollback shows the run's
                    // output alongside any other eval. Cross-workspace
                    // routing mirrors the `ReplEvalDone` handler above:
                    // if the eval was started in a different workspace,
                    // splice into that workspace's snapshot instead of
                    // the live log.
                    // Peek (don't remove): for a streaming run the acceptance ack
                    // arrives before any frame, so removing the key here would
                    // orphan a swapped-away eval's frames. The Done frame drops
                    // the key. Legacy/Err paths remove inline below.
                    let owner = self.eval_id_workspace.get(&eval_id).cloned();
                    let active_key = self.current_workspace_key();
                    match &result {
                        Ok(info) => {
                            let frames = info.frames.clone();
                            let elapsed = info.elapsed_ms;
                            let basename = info
                                .path
                                .rsplit(['/', '\\'])
                                .next()
                                .unwrap_or(info.path.as_str())
                                .to_string();
                            // ADR 0009 phase-2: an empty-frames, 0-elapsed Ok is an
                            // early *acceptance* ack — the run was queued, not yet
                            // executed (so elapsed can only be 0). The streamed
                            // `Done` frame owns completion: it finalizes the entry,
                            // drops the routing key, and sets the final status with
                            // the real elapsed. Here we only stash the display info
                            // (the ack carries the resolved project_dir; the Done
                            // frame doesn't) and show a transient "running" line. A
                            // legacy synchronous-collect Ok finalizes inline.
                            if frames.is_empty() && elapsed == 0 {
                                self.repl_runfile_status.insert(
                                    eval_id,
                                    (basename.clone(), info.project_dir.clone(), info.fresh),
                                );
                                self.status = if info.fresh {
                                    let proj =
                                        info.project_dir.as_deref().unwrap_or("(no project)");
                                    format!("running '{basename}' (fresh — project: {proj})…")
                                } else {
                                    format!("running '{basename}' (existing repl)…")
                                };
                            } else {
                                self.eval_id_workspace.remove(&eval_id);
                                match owner.as_deref() {
                                    Some(key) if key != active_key.as_str() => {
                                        if let Some(snap) =
                                            self.workspace_repl_snapshots.get_mut(key)
                                        {
                                            if let Some(entry) = snap
                                                .repl_log
                                                .iter_mut()
                                                .find(|e| e.eval_id == eval_id)
                                            {
                                                if !frames.is_empty() {
                                                    entry.frames = frames;
                                                }
                                                entry.elapsed_ms = elapsed;
                                                entry.in_flight = false;
                                            }
                                        }
                                    }
                                    _ => {
                                        if let Some(entry) =
                                            self.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                        {
                                            if !frames.is_empty() {
                                                entry.frames = frames;
                                            }
                                            entry.elapsed_ms = elapsed;
                                            entry.in_flight = false;
                                        }
                                    }
                                }
                                self.status = if info.fresh {
                                    let proj =
                                        info.project_dir.as_deref().unwrap_or("(no project)");
                                    format!(
                                        "ran '{basename}' (fresh — project: {proj}, {elapsed}ms)"
                                    )
                                } else {
                                    format!("ran '{basename}' (existing repl, {elapsed}ms)")
                                };
                            }
                            self.window.request_redraw();
                        }
                        Err(msg) => {
                            tracing::warn!(error = %msg, "repl.run_file failed");
                            // The run failed to start — terminal, no Done frame
                            // will follow, so drop the routing key here.
                            self.eval_id_workspace.remove(&eval_id);
                            // Mark the pre-registered entry done with an
                            // error frame so the drawer reflects the
                            // failure instead of spinning forever.
                            let err_frame = sot_protocol::ReplFrame::Error {
                                message: msg.clone(),
                                stacktrace: Vec::new(),
                            };
                            match owner.as_deref() {
                                Some(key) if key != active_key.as_str() => {
                                    if let Some(snap) = self.workspace_repl_snapshots.get_mut(key) {
                                        if let Some(entry) =
                                            snap.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                        {
                                            entry.frames.push(err_frame);
                                            entry.in_flight = false;
                                        }
                                    }
                                }
                                _ => {
                                    if let Some(entry) =
                                        self.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                    {
                                        entry.frames.push(err_frame);
                                        entry.in_flight = false;
                                    }
                                }
                            }
                            self.status = format!("repl.run_file failed · {msg}");
                            self.window.request_redraw();
                        }
                    }
                }
                crate::transport::IncomingEvt::Workspaces { workspaces } => {
                    // ADR 0014: Sessions mode now reads from the daemon's
                    // workspace registry rather than scanning tmux for the
                    // `sot-be-` prefix. Each row carries the canonical
                    // workspace_id + slug in its payload so the swap
                    // handler doesn't have to parse a session name back
                    // into a slug.
                    //
                    // Refresh the slug→label cache used by the connection
                    // status line so a workspace switch shows the friendly
                    // label (e.g. "Alpha") rather than the raw slug. Also
                    // re-populate the ordered slug list + default slug
                    // that drive the Ctrl+PgUp/PgDn cycle hotkey (D7) so
                    // the cycle order matches whatever the backend last
                    // reported (alphabetical).
                    self.workspace_slugs.clear();
                    self.default_workspace_slug = None;
                    self.workspace_project_roots.clear();
                    self.workspace_autostart.clear();
                    self.workspace_states.clear();
                    for w in &workspaces {
                        let label = if w.label.is_empty() {
                            w.slug.clone()
                        } else {
                            w.label.clone()
                        };
                        self.workspace_labels.insert(w.slug.clone(), label);
                        self.workspace_project_roots
                            .insert(w.slug.clone(), w.project_root.clone());
                        // Work-state for the bottom strip's per-name colour.
                        self.workspace_states.insert(
                            w.slug.clone(),
                            (w.agent_state.clone(), w.agent_status_at.clone()),
                        );
                        // Status-change flash (ADR 0023): a slug that had a
                        // *known, different* prior state just transitioned —
                        // stamp a flash so its name blinks in nav + strip.
                        // First-ever appearance (no prior entry) does NOT
                        // flash; `prev_workspace_states` persists across
                        // events (it isn't cleared above) so first-seen is
                        // distinguishable from a real change.
                        match self.prev_workspace_states.get(&w.slug) {
                            Some(prev) if prev != &w.agent_state => {
                                self.flash_starts
                                    .insert(w.slug.clone(), std::time::Instant::now());
                            }
                            _ => {}
                        }
                        self.prev_workspace_states
                            .insert(w.slug.clone(), w.agent_state.clone());
                        self.workspace_slugs.push(w.slug.clone());
                        // Contract (b): remember per-session auto-start info so
                        // attach_session_to_bl can launch claude (ccb) on first
                        // attach. Keyed by tmux_session (what attach uses).
                        //
                        // The persisted spawn brief (`w.task`) is deliberately
                        // DROPPED here (maintainer directive, 2026-06-16): comm-spawn
                        // now sends task:"" and routes --task as a durable
                        // post-spawn comm message (backend 3bcfc63), so FE
                        // brief-delivery is pure redundancy. Worse, the FE
                        // re-delivered the stored task on every re-attach/switch —
                        // after a relaunch forgot `autostarted_sessions` —
                        // re-pasting a stale brief into an already-running agent
                        // (a stale brief re-injected on a switch
                        // → the agent choked with a 529). Forcing it empty here
                        // neutralizes every pre-today workspace's persisted task
                        // with no per-workspace purge or daemon bounce; ccb's own
                        // /sot-session-start bootstrap remains the agent's init.
                        // (`autostart_claude_in_pane` then takes its launch-only
                        // path, never creating an `AutoStartDelivery`.)
                        self.workspace_autostart.insert(
                            w.tmux_session.clone(),
                            WsAutostart {
                                autostart_claude: w.autostart_claude,
                                agent_name: w.agent_name.clone(),
                                task: String::new(),
                            },
                        );
                        if w.is_default {
                            self.default_workspace_slug = Some(w.slug.clone());
                        }
                    }
                    self.rebuild_connection_status();
                    // --capture-cycle <N>: simulate N Ctrl+PgDn presses
                    // (negative = Ctrl+PgUp) on the first workspace.list
                    // reply. Consumed once so a re-fetch from a later
                    // switch doesn't re-cycle.
                    if self.capture_cycle != 0 {
                        let steps = self.capture_cycle;
                        self.capture_cycle = 0;
                        let dir = if steps > 0 { 1 } else { -1 };
                        for _ in 0..steps.abs() {
                            self.cycle_workspace(dir);
                        }
                    }
                    // The rest of this handler rebuilds the Sessions-mode
                    // tree. Skip it when the chrome is showing a different
                    // mode: a `workspace.list` refresh from switch_to_workspace
                    // (kernel_running etc.) would otherwise clobber the
                    // active workspace's Files / Modules tree with the
                    // workspaces-list rows.
                    if !matches!(self.mode, Mode::Sessions) {
                        continue;
                    }
                    let root = TreeNode {
                        id: "sessions:".to_string(),
                        label: "workspaces".to_string(),
                        kind: "sessions".to_string(),
                        has_children: true,
                        badges: Vec::new(),
                        payload: Default::default(),
                    };
                    let create_row = TreeNode {
                        id: "sessions:+create".to_string(),
                        label: "[+ create new]".to_string(),
                        kind: "session_create".to_string(),
                        has_children: false,
                        badges: Vec::new(),
                        payload: Default::default(),
                    };
                    let mut children = vec![create_row];
                    children.extend(workspaces.into_iter().map(|w| {
                        let mut payload = serde_json::Map::new();
                        // `name` stays the tmux session name so the
                        // existing `selected_session_name` /
                        // `attach_session_to_bl` paths work unchanged.
                        payload.insert(
                            "name".to_string(),
                            serde_json::Value::String(w.tmux_session.clone()),
                        );
                        payload.insert(
                            "workspace_id".to_string(),
                            serde_json::Value::String(w.workspace_id.clone()),
                        );
                        payload.insert(
                            "slug".to_string(),
                            serde_json::Value::String(w.slug.clone()),
                        );
                        payload.insert(
                            "label".to_string(),
                            serde_json::Value::String(w.label.clone()),
                        );
                        payload.insert(
                            "project_root".to_string(),
                            serde_json::Value::String(w.project_root.clone()),
                        );
                        payload.insert(
                            "kernel_running".to_string(),
                            serde_json::Value::Bool(w.kernel_running),
                        );
                        payload.insert(
                            "is_default".to_string(),
                            serde_json::Value::Bool(w.is_default),
                        );
                        // State-nav (ADR 0023): carry the agent work-state +
                        // status timestamp so the render step can colour the
                        // row and age a stale "working". The summary itself
                        // rides in the label (the glance line); these two are
                        // for styling only.
                        payload.insert(
                            "agent_state".to_string(),
                            serde_json::Value::String(w.agent_state.clone()),
                        );
                        payload.insert(
                            "agent_status_at".to_string(),
                            serde_json::Value::String(w.agent_status_at.clone()),
                        );
                        let mut badges = Vec::new();
                        if w.is_default {
                            badges.push("default".to_string());
                        }
                        if w.kernel_running {
                            badges.push("kernel".to_string());
                        }
                        // The glance line. With agent state present, the
                        // agent's one-line summary is the default at-a-glance
                        // text (what it's doing now / just finished) — far more
                        // useful per-session than the project path. Falls back
                        // to the state word if the summary is empty, and to the
                        // project root when there's no agent state at all
                        // (renders exactly as it did pre-state-nav).
                        let glance = if !w.agent_summary.is_empty() {
                            w.agent_summary.clone()
                        } else if !w.agent_state.is_empty() {
                            w.agent_state.clone()
                        } else {
                            w.project_root.clone()
                        };
                        let label = if w.label.is_empty() {
                            w.slug.clone()
                        } else {
                            format!("{} · {}", w.label, glance)
                        };
                        TreeNode {
                            id: format!("sessions:{}", w.tmux_session),
                            label,
                            kind: "session".to_string(),
                            has_children: true,
                            badges,
                            payload,
                        }
                    }));
                    self.tree.set_root(root, children);
                    if matches!(self.mode, Mode::Sessions) {
                        if let Some(n) = self.pending_initial_selection.take() {
                            self.tree.selected = n.min(self.tree.rows.len().saturating_sub(1));
                        }
                    }
                }
            }
        }
    }

    fn resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);
        self.text
            .resize(&self.queue, self.config.width, self.config.height);

        let (cols, rows) = cell_grid_for(
            self.config.width,
            self.config.height,
            self.cell_w,
            self.cell_h,
            self.chrome_origin_x,
            self.chrome_origin_y,
        );
        self.terminal.backend_mut().resize(cols, rows);
        // ratatui needs to know the grid changed so it reallocates its buffers.
        let _ = self
            .terminal
            .resize(ratatui::layout::Rect::new(0, 0, cols, rows));
    }

    /// Refresh the cached battery label if it's stale (or never queried).
    /// The OS query (via the cross-platform `battery` crate) is not free, so
    /// it runs at most once per `BATTERY_QUERY_INTERVAL`; between refreshes the
    /// per-second clock repaint reuses the cached value. On no battery present
    /// (desktop / CI) or any query error we set the label to `None` so the
    /// chrome paints nothing for the battery — never a fake `0%` or `N/A`.
    fn refresh_battery_label(&mut self) {
        /// How often to hit the OS for a fresh battery reading.
        const BATTERY_QUERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

        let now = std::time::Instant::now();
        let fresh = self
            .last_battery_query
            .map(|t| now.duration_since(t) < BATTERY_QUERY_INTERVAL)
            .unwrap_or(false);
        if fresh {
            return;
        }
        self.last_battery_query = Some(now);
        self.battery_label = query_battery_label();
    }

    /// Status-change flash factor for `slug` at `now` (1.0 right at the
    /// transition, fading to 0.0 over `FLASH_SECS`). `0.0` when the slug has
    /// no live flash. Pure read so both the (immutable-borrow) nav-row build
    /// and the strip caller can use it.
    fn flash_factor_for(&self, slug: &str, now: std::time::Instant) -> f32 {
        self.flash_starts
            .get(slug)
            .map(|t| flash_factor(now.duration_since(*t).as_secs_f32()))
            .unwrap_or(0.0)
    }

    /// Drop `flash_starts` entries whose fade has fully elapsed so the map
    /// stays small and `about_to_wait` stops scheduling fast repaints once no
    /// flash is live. Returns true while any flash is still animating.
    fn prune_expired_flashes(&mut self, now: std::time::Instant) -> bool {
        let window = std::time::Duration::from_secs_f32(FLASH_SECS);
        self.flash_starts
            .retain(|_, t| now.duration_since(*t) < window);
        !self.flash_starts.is_empty()
    }

    fn redraw(&mut self) -> Result<()> {
        self.drain_events();
        // Prune finished status-change flashes; while any is still fading,
        // mark dirty so the frame loop keeps animating it (the fast-repaint
        // cadence is armed in `about_to_wait`).
        if self.prune_expired_flashes(std::time::Instant::now()) {
            self.dirty = true;
        }
        // Decide any armed pre-launch sniff (contract b): skip the launch if
        // the pane already runs claude, else fire it. Must run before
        // advance_delivery (it's what creates the delivery on launch).
        self.advance_autostart_scan();
        // Advance any in-flight agent auto-start task delivery (contract b).
        // Runs each frame/tick — the idle clock tick guarantees ~1s cadence —
        // watching the pinned pane for output to settle before each keystroke.
        self.advance_delivery();
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
            self.maybe_fire_tmux_capture();
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
        // fire exactly as they would for a user descent — which is the
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

        // When a NavTree text prompt is up, the status line becomes the
        // prompt's input field so the user sees what they're typing — least
        // invasive spot (no extra chrome row, no layout shift). A block
        // cursor (▏) marks the insertion point.
        let status = match &self.nav_prompt {
            Some(NavPrompt::CreateFile { input, .. }) => format!("new file: {input}▏"),
            Some(NavPrompt::ConfirmDelete { label, .. }) => {
                format!("delete {label}? [y/N]")
            }
            None => self.status.clone(),
        };
        // Local wall-clock of the machine running the frontend, 24-hour
        // HH:MM:SS, painted into the top-right chrome corner. `chrono::Local`
        // is cross-platform (same behaviour on Windows/macOS/Linux); the
        // once-per-second repaint is scheduled in `about_to_wait`.
        let clock = chrono::Local::now().format("%H:%M:%S").to_string();
        // Battery readout painted just left of the clock. The OS query isn't
        // free, so refresh the cache at most once per `BATTERY_QUERY_INTERVAL`
        // (the clock repaints ~1×/s and reuses the cached value between
        // refreshes). `None` => no battery / query failed => paint nothing.
        self.refresh_battery_label();
        let battery = self.battery_label.clone();
        let last_key = self.last_key.clone();
        let mode = self.mode;
        let focus = self.focus;
        let maximized = self.maximized;
        // State-nav selected-session contrast lever, snapshotted for the draw
        // closure (it mustn't borrow `self`).
        let contrast_dim = self.contrast_dim;
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
        let (repl_lines, repl_slots) = build_repl_lines(
            &self.repl_log,
            &self.repl_images,
            self.repl_scrollback_px.w,
            self.repl_scrollback_px.h,
            self.cell_w,
            self.cell_h,
        );
        self.repl_image_slots = repl_slots;
        let repl_input = self.repl_input.clone();
        let repl_pkg_mode = self.repl_pkg_mode;
        // Snapshot for nav scroll calc: body_lines lays out as
        // [status, hint, blank, (tree rows...), blank, concept, blank, key],
        // so the cursor's body index is 3 + tree.selected when the tree
        // isn't empty.
        // Pick the cursor position from the picker when it's active so
        // the scroll math keeps the highlighted picker row in the
        // comfort zone. body_lines starts with 3 chrome header lines
        // (status / hint / blank), then tree_lines. Files-mode's first
        // tree row is therefore at body row 3 (+ self.tree.selected).
        // The picker prepends 2 extra title rows (path header + key
        // hint) inside tree_lines, so the first picker entry sits at
        // body row 3 + 2 = 5 (+ picker.selected).
        let (nav_cursor_body_pos, nav_has_cursor) = match &self.workspace_picker {
            Some(p) => (5usize.saturating_add(p.selected), !p.entries.is_empty()),
            None => (
                3usize.saturating_add(self.tree.selected),
                !self.tree.rows.is_empty(),
            ),
        };
        // Mutable copy of the persistent scroll. The draw closure updates
        // this in place based on the cursor's viewport position; the
        // result is written back to self.tree_scroll after the draw.
        let mut nav_scroll = self.tree_scroll;
        // Apply the LLM-pane scrollback offset *before* taking an
        // immutable borrow for the draw. set_scrollback saturates
        // internally when the requested offset exceeds the buffered
        // rows, so we read the actual offset back to keep State in
        // sync with what the emulator agreed to.
        self.pty_terminal
            .screen_mut()
            .set_scrollback(self.pty_scroll as usize);
        let actual_pty_scroll = self.pty_terminal.screen().scrollback();
        self.pty_scroll = actual_pty_scroll.min(u16::MAX as usize) as u16;
        // Local terminal drawer (G2/G3): lazily spawn the OS shell the
        // first time the Terminal drawer is shown, then drain any pending
        // output into its parser before we borrow its screen for the draw.
        // All mutation happens here, before the `self.terminal.draw`
        // borrow and the immutable `local_term` screen borrow below.
        if self.drawer == DrawerContent::Terminal {
            if self.local_term.is_none() {
                let shell = crate::term::resolve_shell(self.settings.terminal_shell.as_deref());
                let waker = self.window.clone();
                // cwd = repo root (so `claude --continue` finds the project's
                // session); resume command runs only on the first spawn after
                // a `--relaunched` start, then is cleared. ADR 0017.
                let cwd = self.repo_dir.clone();
                let resume = self.pending_resume_command.take();
                match crate::term::LocalTerminal::spawn(
                    &shell,
                    80,
                    24,
                    cwd.as_deref(),
                    resume.as_deref(),
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
                // Apply the scrollback offset before borrowing the screen for
                // the draw, then read back the clamped value (the ring may
                // hold fewer rows than requested) — mirrors the pty pane.
                t.screen_mut().set_scrollback(self.term_scroll as usize);
                self.term_scroll = t.screen().scrollback().min(u16::MAX as usize) as u16;
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
        // vt100::Parser's screen() returns a `&Screen` tied to the
        // parser; since `terminal.draw` borrows a different field
        // (self.terminal, not self.pty_terminal), Rust's split-borrow
        // rules let us hold both at once. The local terminal's screen is
        // borrowed the same way when the Terminal drawer is active.
        let pty_screen = self.pty_terminal.screen();
        let term_screen = if drawer == DrawerContent::Terminal {
            self.local_term.as_ref().map(|t| t.screen())
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
        // (text, is_selected, is_stale, is_pinned, agent_tone, flash, is_pending)
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
        );
        let (tree_lines, tree_empty): (Vec<NavRow>, bool) = if let Some(p) = &self.workspace_picker
        {
            let mut rows: Vec<NavRow> = Vec::with_capacity(p.entries.len() + 2);
            rows.push((
                format!("workspace picker · {}", p.current_path),
                false,
                false,
                false,
                None,
                0.0,
                false,
            ));
            // Two footer rows: NAVIGATION first (→ is how you descend into
            // a folder — Enter does NOT, it creates), then the create keys.
            // Splitting them stops the common muscle-memory error of hitting
            // Enter to open a folder and instead spawning a session.
            rows.push((
                "  → into folder · ← up · ↑↓ move · Esc cancel".to_string(),
                false,
                false,
                false,
                None,
                0.0,
                false,
            ));
            rows.push((
                "  Enter = create here · Shift+Enter bare · Ctrl+Enter codex".to_string(),
                false,
                false,
                false,
                None,
                0.0,
                false,
            ));
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
                    let stale = selected && selected_stale;
                    let pinned = pinned_id == Some(r.node.id.as_str());
                    // Agent tone + status-change flash only on Sessions
                    // rows (kind "session"), keyed by the row's slug so it
                    // matches the bottom strip. `pending` (badge floor, ADR
                    // 0025 §1) is set when that workspace has a pending
                    // nav.preview result waiting, keyed by the same slug.
                    let (agent, flash, pending) = if r.node.kind == "session" {
                        let slug = r.node.payload.get("slug").and_then(|v| v.as_str());
                        let flash = slug
                            .map(|s| self.flash_factor_for(s, flash_now))
                            .unwrap_or(0.0);
                        let pending = slug
                            .map(|s| self.pending_nav.contains_key(s))
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
                    )
                })
                .collect();
            let empty = self.tree.rows.is_empty();
            (rows, empty)
        };
        // Layout proportions from the user's settings file (or
        // defaults). Snapshotted here so the ratatui closure doesn't
        // borrow `self`. Maximisation overrides the geom inside the
        // closure by passing a `maximize_slot`.
        let layout_preset = self.settings.resolve_preset(self.monitor_aspect).clone();
        // `drawer` was bound above (after the terminal lazy-spawn/close).
        let drawer_open = drawer.is_open();
        // T1: full path of the file the preview is showing, snapshotted here
        // so the draw closure doesn't borrow `self`.
        let preview_name = self.preview_pane_name();
        // Sessions create-legend gate: is the workspace picker open? Snapshotted
        // here (Copy bool) so the header inside the draw closure can decide
        // whether to show the standalone three-key create legend without
        // borrowing `self`. Suppressed while the picker is open — the picker's
        // own footer already carries the legend inline.
        let picker_open = self.workspace_picker.is_some();

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
                // backend safe). Toggle: Ctrl+z.
                let maximize_slot = if maximized {
                    Some(match focus {
                        PaneFocus::NavTree => crate::settings::Slot::Nav,
                        PaneFocus::Preview => crate::settings::Slot::Preview,
                        PaneFocus::Llm => crate::settings::Slot::Llm,
                        PaneFocus::Repl => crate::settings::Slot::Repl,
                    })
                } else {
                    None
                };
                let geom = crate::layout::compute(area, &layout_preset, drawer_open, maximize_slot);
                // Names preserved so the rest of the closure reads
                // unchanged: nav = old TL (left column), preview = old
                // TR (middle column), llm = old BL (rightmost column
                // in the 3-col layout), repl = old BR (bottom drawer).
                let nav_rect = geom.rect_for(crate::settings::Slot::Nav);
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
                // Header leads with the SURVIVAL keys (maintainer note, 2026-07-04): `?`
                // only works with nav focus, so a user stuck in another pane
                // needs the pane-switch chord permanently visible — this line
                // is the way back. Everything else lives in the `?` overlay
                // (rebuild_help_overlay); the old two-row inline hints clipped
                // on narrow nav columns, so this stays one short row.
                let mut body_lines = vec![RtLine::from(vec![Span::styled(
                    "  Ctrl+←→↑↓ switch pane · ↑↓ move · ? help",
                    Style::default().fg(Color::LightCyan),
                )])];
                // Sessions mode: the three-key create legend (ADR 0031), shown
                // the moment you switch into Sessions mode so the agent choice
                // is visible without opening `?` or drilling into the picker.
                // Suppressed while the picker is open — its footer carries the
                // same legend inline (below). One short row; the nav body does
                // not wrap, so keep it inside a narrow column.
                // Tree-nav movement legend + (Sessions only) the create legend.
                // Files / Modules / Sessions share the same →into / ←up / ↑↓move
                // gestures, so the movement row shows in all three — it makes →
                // discoverable as "descend" and stops Enter from being mistaken
                // for it (in Files, Enter opens; in Sessions' picker, Enter
                // creates). The three-key create legend is Sessions-only.
                // Suppressed in the picker — its footer carries both inline.
                let tree_nav = matches!(mode, Mode::Files | Mode::Modules | Mode::Sessions);
                if tree_nav && !picker_open {
                    if mode == Mode::Sessions {
                        body_lines.push(RtLine::from(vec![Span::styled(
                            "  new: Enter claude · Shift+Enter bare · Ctrl+Enter codex",
                            Style::default().fg(Color::DarkGray),
                        )]));
                    }
                    body_lines.push(RtLine::from(vec![Span::styled(
                        "  move: → into · ← up · ↑↓ rows",
                        Style::default().fg(Color::DarkGray),
                    )]));
                }
                body_lines.push(RtLine::from(vec![
                    Span::styled("status: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(status.clone(), Style::default().fg(Color::LightGreen)),
                ]));
                body_lines.push(RtLine::from(""));
                if tree_empty {
                    body_lines.push(RtLine::from(vec![Span::styled(
                        "  (no tree yet)",
                        Style::default().add_modifier(Modifier::DIM),
                    )]));
                }
                for (text, is_selected, is_stale, is_pinned, agent, flash, is_pending) in
                    &tree_lines
                {
                    let mut style = Style::default();
                    // Cross-cutting colour layer. State-nav agent tone (ADR
                    // 0023) owns the colour of a Sessions row that has one:
                    // working/idle/blocked/done each get a hue, a stale
                    // "working" wilts (DIM), and selection still reads through
                    // the `>` caret + bold so the cursor stays visible over
                    // the state colour. Without an agent tone we fall back to
                    // the original layer: stale (annotation drift) is loudest,
                    // then the pinned accent (bright cyan, distinct from the
                    // yellow stale/selected hues), then selection (light
                    // yellow), then dim.
                    if let Some((tone, aged)) = agent {
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
                    } else if *is_stale {
                        style = style.fg(Color::Yellow);
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
                let nav_inner_h = nav_rect.height as usize;
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
                    nav: nav_rect,
                    preview: preview_rect,
                    llm: llm_rect,
                    repl: repl_rect,
                };

                let llm_focus = focus == PaneFocus::Llm;
                let llm_title = if llm_focus {
                    " llm · tmux · [FOCUS] ".to_string()
                } else {
                    " llm · tmux ".to_string()
                };

                let repl_focus = focus == PaneFocus::Repl;
                // G6: the drawer title reflects which content it's showing —
                // the Julia REPL (Ctrl+J) or the local terminal (Ctrl+T).
                let repl_title = match (drawer, repl_focus) {
                    (DrawerContent::Terminal, true) => " terminal · [FOCUS] ".to_string(),
                    (DrawerContent::Terminal, false) => " terminal ".to_string(),
                    (DrawerContent::Monitor, _) => " monitor ".to_string(),
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
                frame.render_widget(nav_body, nav_rect);
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
                if nav_rect.width > 0 {
                    write_title(
                        buf,
                        nav_rect.x + 1,
                        nav_rect.y.saturating_sub(1),
                        &nav_title,
                        title_w(nav_rect),
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

                // Live local-time clock, right-aligned on the top edge just
                // inside the outer-right corner glyph. Same chrome text style
                // as an idle pane title. Repaints ~1×/second via the
                // `about_to_wait` WaitUntil scheduling below.
                {
                    let clock_label = format!(" {clock} ");
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

                // LLM pane: paint the vt100 terminal grid into the
                // BL content rect. Walk every cell of the emulator
                // screen at (row, col), look up its glyph + colour,
                // and write into the chrome buffer at the matching
                // (llm_rect.x + col, llm_rect.y + row). The emulator
                // was sized to llm_rect earlier, so the grid fits
                // exactly.
                paint_terminal(buf, llm_rect, &pty_screen);
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

        // Open the LLM-pane pty on the first redraw that has a real
        // BL content rect, or resize it when the rect grew/shrank.
        // `tmux new-session -A -s sot-llm` is what runs on the
        // backend side, so this is idempotent across launches.
        let (cols, rows) = pty_size_observed;
        if cols >= 2 && rows >= 2 {
            let need_open = self.pty_size.is_none();
            let need_resize = self
                .pty_size
                .map(|prev| prev != (cols, rows))
                .unwrap_or(false);
            if need_open {
                if let Err(e) = self.req_tx.send(OutgoingReq::PtyOpen {
                    cols,
                    rows,
                    target: self.bl_pane_target.clone(),
                    // #5 guard: the first-redraw initial BL open is startup
                    // restore, not a user switch — false so it can't claim the
                    // foreground from where the user (on any FE) put it.
                    user_switch: false,
                }) {
                    tracing::warn!(error = %e, "drop pty.open request — channel closed");
                } else {
                    // Locally seed the size so a resize doesn't fire
                    // before the response confirms. The emulator is
                    // resized to match so we don't render at the
                    // wrong dims before the first bytes arrive.
                    self.pty_terminal.screen_mut().set_size(rows, cols);
                    self.pty_size = Some((cols, rows));
                }
            } else if need_resize {
                if let Err(e) = self.req_tx.send(OutgoingReq::PtyResize { cols, rows }) {
                    tracing::warn!(error = %e, "drop pty.resize — channel closed");
                } else {
                    self.pty_terminal.screen_mut().set_size(rows, cols);
                    self.pty_size = Some((cols, rows));
                    // Row layout shifted; the previous scrollback offset
                    // no longer points where the user expected. Snap to
                    // live rather than show a confused slice.
                    self.pty_scroll = 0;
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
        let border_thickness = (self.cell_h * 0.09).round().max(1.0);
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
        // Miniature brand-logo rects flanking each badge are accumulated here
        // (physical px) and drawn from `self.logo_quad` inside the render pass
        // below — the per-badge layout they need only exists in this block.
        let mut strip_logo_rects: Vec<ScreenRect> = Vec::new();
        if !self.workspace_slugs.is_empty() {
            let labels: Vec<String> = self
                .workspace_slugs
                .iter()
                .map(|s| {
                    strip_truncate(
                        self.workspace_labels
                            .get(s)
                            .map(String::as_str)
                            .unwrap_or(s.as_str()),
                    )
                })
                .collect();
            let current = self
                .active_workspace_id
                .clone()
                .or_else(|| self.default_workspace_slug.clone());
            let active = current
                .as_deref()
                .and_then(|s| self.workspace_slugs.iter().position(|x| x == s))
                .unwrap_or(0)
                .min(labels.len().saturating_sub(1));
            let target = session_strip_target(&labels, active, self.cell_w);
            let scroll = self.strip_scroll_px.unwrap_or(target);
            let baseline_y = (self.config.height as f32 - self.cell_h - 2.0).max(0.0);
            // Per-name work-state tone, parallel to `labels` (built from
            // `workspace_slugs` in the same order). `now` is fetched per frame
            // so the wilt re-evaluates on the existing 1 Hz idle redraw.
            let strip_now = chrono::Utc::now();
            let tones: Vec<Option<(AgentTone, bool)>> = self
                .workspace_slugs
                .iter()
                .map(|s| {
                    self.workspace_states
                        .get(s)
                        .and_then(|(st, at)| agent_tone_from(st, at, strip_now))
                })
                .collect();
            // Per-name status-change flash factor, parallel to `labels`.
            let flash_now = std::time::Instant::now();
            let flashes: Vec<f32> = self
                .workspace_slugs
                .iter()
                .map(|s| self.flash_factor_for(s, flash_now))
                .collect();
            // Per-name badge-floor pending flag, parallel to `labels` (ADR
            // 0025 §1): true when that workspace has a pending nav.preview
            // result waiting.
            let pendings: Vec<bool> = self
                .workspace_slugs
                .iter()
                .map(|s| self.pending_nav.contains_key(s))
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
            );
            // Bookend the whole row of session names with the dark logo: one
            // mini logo just left of the leftmost visible badge, one just right
            // of the rightmost. (Per-badge flanking was too crowded — logo-dark
            // is square ~2 cells wide and the inter-badge gap is only 3 cells,
            // so adjacent flankers overlapped.) Each strip Line carries `.x`
            // (badge left) and `.text`; badge width = chars * cell_w, matching
            // session_strip_lines' own layout math. Rects are drawn from
            // `self.logo_quad` via render_many in the pass below. A bookend that
            // would spill off its window edge is skipped.
            if let Some((_, nw, nh)) = self.logo_quad.as_ref() {
                if !strip_lines.is_empty() {
                    let win_w = self.config.width as f32;
                    let logo_h = (self.cell_h - 2.0).max(1.0);
                    let logo_w = logo_h * (*nw as f32 / (*nh).max(1) as f32);
                    let pad = 2.0 * self.cell_w; // breathing room between logo and names (2 cells)
                    let mut min_left = f32::MAX;
                    let mut max_right = f32::MIN;
                    for ln in &strip_lines {
                        let w = ln.text.chars().count() as f32 * self.cell_w;
                        min_left = min_left.min(ln.x);
                        max_right = max_right.max(ln.x + w);
                    }
                    let lx = min_left - pad - logo_w;
                    if lx >= 0.0 {
                        strip_logo_rects.push(ScreenRect {
                            x: lx,
                            y: baseline_y,
                            w: logo_w,
                            h: logo_h,
                        });
                    }
                    let rx = max_right + pad;
                    if rx + logo_w <= win_w {
                        strip_logo_rects.push(ScreenRect {
                            x: rx,
                            y: baseline_y,
                            w: logo_w,
                            h: logo_h,
                        });
                    }
                }
            }
            lines.extend(strip_lines);
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
        let show_fatal = self.protocol_mismatch.is_some();
        // Help overlay takes the preview pane over everything else when open.
        let show_help = !show_fatal && self.help_open && self.preview_help.is_some();
        let show_png = self.preview_png.is_some() && !show_help && !show_fatal;
        let show_svg = false;
        let show_concept = false;
        // Edit mode owns the preview pane: the file viewer hides so
        // the editable annotation body has the whole rect.
        let show_edit =
            !show_fatal && !show_help && self.edit_state.is_some() && self.preview_edit.is_some();
        let show_md = !show_fatal && !show_help && !show_png && !show_edit;

        let png_rect = if show_png {
            self.preview_png
                .as_ref()
                .map(|q| letterbox(preview_rect, q.size_px))
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
        // `--start-help` (capture harness) opens the overlay before any `?`
        // press. Build it lazily here, once md_rect_px reflects the real
        // preview width so the cheat sheet wraps right; renders next frame.
        if self.help_open && self.preview_help.is_none() {
            self.rebuild_help_overlay();
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
        // When both md and concept render, clamp by the taller so
        // neither hits its bottom before the other has been fully
        // reached.
        let line_h = self.preview_md.line_height().max(1.0);
        let md_total_px = self.preview_md.total_visual_pixels(line_h);
        let concept_total_px = self
            .preview_concept
            .as_ref()
            .map(|p| p.total_visual_pixels(line_h))
            .unwrap_or(0.0);
        let total_px = md_total_px.max(concept_total_px);
        // The extras paint with `EXTRA_TOP_PAD_PX` of headroom, so each
        // frame only renders `md_rect.h - pad` pixels of content. Subtract
        // the pad from `visible_px` so max_scroll lets the user reach the
        // actual bottom of the document without losing the tail to the
        // padding.
        let visible_px = (md_rect.h - crate::text::EXTRA_TOP_PAD_PX).max(line_h);
        let max_scroll_px = (total_px - visible_px).max(0.0);
        // `preview_scroll` is body-line units; convert the pixel slack
        // back via ceil so the final body-line step always lands the
        // bottom of the document on screen (no off-by-fraction clip).
        let max_scroll = (max_scroll_px / line_h).ceil() as u16;
        self.preview_scroll = self.preview_scroll.min(max_scroll);
        let preview_scroll_px = self.preview_scroll as f32 * line_h;

        // Walk the laid-out markdown buffer for FFFC placeholders, zip
        // with `preview_md.media_blocks` by appearance order, and
        // pre-rasterise any math SVGs we have that haven't been
        // rasterised yet at the current pane width. Painting happens
        // inside the rpass below; do the side-effecty rasterise here
        // while we have &mut self.
        let media_paint_targets: Vec<(usize, ScreenRect)> = if show_md {
            self.collect_media_paint_targets(preview_rect, preview_scroll_px)
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

        let mut extras: Vec<crate::text::ExtraArea> = Vec::new();
        if show_md {
            extras.push(crate::text::ExtraArea {
                buffer: &self.preview_md.buffer,
                x: md_rect.x,
                y: md_rect.y,
                right: md_rect.x + md_rect.w,
                bottom: md_rect.y + md_rect.h,
                clip_left: None,
                clip_top: None,
                color: (220, 220, 220),
                scroll_y_px: preview_scroll_px,
            });
        }
        if show_concept {
            if let Some(pc) = self.preview_concept.as_ref() {
                extras.push(crate::text::ExtraArea {
                    buffer: &pc.buffer,
                    x: concept_rect.x,
                    y: concept_rect.y,
                    right: concept_rect.x + concept_rect.w,
                    bottom: concept_rect.y + concept_rect.h,
                    clip_left: None,
                    clip_top: None,
                    // Slight magenta tint so the annotation reads as the
                    // "concept layer" content even when sharing the slot.
                    color: (220, 200, 230),
                    scroll_y_px: preview_scroll_px,
                });
            }
        }
        if show_edit {
            if let Some(pe) = self.preview_edit.as_ref() {
                extras.push(crate::text::ExtraArea {
                    buffer: &pe.buffer,
                    x: preview_rect.x,
                    y: preview_rect.y,
                    right: preview_rect.x + preview_rect.w,
                    bottom: preview_rect.y + preview_rect.h,
                    clip_left: None,
                    clip_top: None,
                    // Warm gold tint so the user sees at a glance that
                    // this is editable, not the read-only annotation.
                    color: (235, 215, 160),
                    scroll_y_px: preview_scroll_px,
                });
            }
        }
        if show_help {
            if let Some(ph) = self.preview_help.as_ref() {
                extras.push(crate::text::ExtraArea {
                    buffer: &ph.buffer,
                    x: preview_rect.x,
                    y: preview_rect.y,
                    right: preview_rect.x + preview_rect.w,
                    bottom: preview_rect.y + preview_rect.h,
                    clip_left: None,
                    clip_top: None,
                    // Cool cyan tint distinguishes the help overlay from the
                    // gold edit modal and the neutral read-only preview.
                    color: (150, 210, 220),
                    scroll_y_px: 0.0,
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
                    bottom: preview_rect.y + preview_rect.h,
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
                    // scroll past the pane edges is invisible.
                    right: preview_rect.x + preview_rect.w,
                    bottom: preview_rect.y + preview_rect.h,
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
                // `(preview_rect, size_px)` inputs as the `letterbox` above,
                // so at the ceiling canvas_w == 16 × source-px exactly.
                let zoom_max = png_zoom_max(preview_rect.w, preview_rect.h, quad.size_px);
                let zoom = self.preview_png_zoom.clamp(1.0, zoom_max);
                self.preview_png_zoom = zoom;
                let canvas_w = letterbox_rect.w * zoom;
                let canvas_h = letterbox_rect.h * zoom;
                let pane_cx = preview_rect.x + preview_rect.w * 0.5;
                let pane_cy = preview_rect.y + preview_rect.h * 0.5;
                // Clamp pan so canvas always covers the pane in any axis
                // where canvas > pane. When canvas < pane (e.g. at zoom
                // 1 with a non-pane-aspect image), pan in that axis is
                // forced to 0 so the letterbox stays centred.
                let slack_x = (canvas_w - preview_rect.w).max(0.0);
                let slack_y = (canvas_h - preview_rect.h).max(0.0);
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
                            preview_rect.x,
                            preview_rect.y,
                            preview_rect.w,
                            preview_rect.h,
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
                let sx = preview_rect.x.max(0.0) as u32;
                let sy = preview_rect.y.max(0.0) as u32;
                let sw = preview_rect.w.max(0.0) as u32;
                let sh = preview_rect.h.max(0.0) as u32;
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

            // Brand chrome: dark logos bookending the bottom session strip, plus
            // the wordmark at the top-right of the nav pane. Drawn just before
            // the text layer so any glyphs (e.g. the nav title) stay legible on
            // top. Cosmetic — each is skipped when its quad failed to decode
            // (field is None).
            //
            // Strip bookends via render_many, NOT a render() loop: render()
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
            // Wordmark — right-aligned on the nav pane's title row, clear of the
            // left-aligned "nav · mode …" title and the "? for help" line below
            // it. One row tall; width follows the PNG's native aspect, clamped so
            // it never eats more than ~45% of the pane width.
            if let Some((quad, nw, nh)) = self.wordmark_quad.as_ref() {
                let nav = self.pane_rects.nav;
                let nav_x = self.chrome_origin_x + nav.x as f32 * self.cell_w;
                let nav_y = self.chrome_origin_y + nav.y as f32 * self.cell_h;
                let nav_w = nav.width as f32 * self.cell_w;
                let mut wm_h = self.cell_h;
                let mut wm_w = wm_h * (*nw as f32 / (*nh).max(1) as f32);
                let max_w = (nav_w * 0.45).max(1.0);
                if wm_w > max_w {
                    wm_w = max_w;
                    wm_h = wm_w * (*nh as f32 / (*nw).max(1) as f32);
                }
                quad.render(
                    &self.queue,
                    &self.quad_pipeline,
                    &mut rpass,
                    ScreenRect {
                        x: nav_x + nav_w - wm_w - self.cell_w,
                        y: nav_y + 1.0,
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

/// Destination for a Ctrl+Shift+S selfie: `<dir>/selfie-<YYYYMMDD-HHMMSS>.png`,
/// where `dir` is `$SOT_SELFIE_DIR`, else `<$SOT_REPO_DIR>/selfies`, else the
/// current working directory. Creates the directory if missing.
fn selfie_path() -> PathBuf {
    let dir = std::env::var_os("SOT_SELFIE_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("SOT_REPO_DIR").map(|r| PathBuf::from(r).join("selfies")))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let _ = std::fs::create_dir_all(&dir);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    dir.join(format!("selfie-{stamp}.png"))
}

/// Schedule a copy of `texture` into a freshly-allocated MAP_READ buffer.
/// The buffer is returned so the caller can submit the encoder, present the
/// frame, then map and decode the buffer once the GPU has finished.
fn stage_capture(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> (wgpu::Buffer, u32, u32) {
    let bpp = 4u32;
    let unpadded_bpr = width * bpp;
    let padded_bpr = (unpadded_bpr + wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1)
        & !(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT - 1);
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("capture-readback"),
        size: (padded_bpr as u64) * (height as u64),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buf,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    (buf, padded_bpr, unpadded_bpr)
}

/// Map the readback buffer, compact padded rows, normalize channel order to
/// RGBA8, and write a PNG. Synchronous: we block on `device.poll(Wait)` since
/// the frontend is exiting after this anyway.
fn finish_capture(
    device: &wgpu::Device,
    buf: wgpu::Buffer,
    padded_bpr: u32,
    unpadded_bpr: u32,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    path: &std::path::Path,
) -> Result<()> {
    let slice = buf.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .context("readback channel closed")?
        .context("map_async failed")?;

    let data = slice.get_mapped_range();
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    let bgra = matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );
    for y in 0..height {
        let row_start = (y * padded_bpr) as usize;
        let row_end = row_start + unpadded_bpr as usize;
        let row = &data[row_start..row_end];
        for px in row.chunks_exact(4) {
            if bgra {
                pixels.push(px[2]);
                pixels.push(px[1]);
                pixels.push(px[0]);
                pixels.push(px[3]);
            } else {
                pixels.push(px[0]);
                pixels.push(px[1]);
                pixels.push(px[2]);
                pixels.push(px[3]);
            }
        }
    }
    drop(data);
    buf.unmap();

    image::save_buffer(path, &pixels, width, height, image::ColorType::Rgba8)
        .with_context(|| format!("save PNG to {}", path.display()))?;
    Ok(())
}

/// Per-machine Ship of Tools state directory: `%LOCALAPPDATA%\sot\` on Windows,
/// `$XDG_STATE_HOME/sot` (or `$HOME/.local/state/sot`) elsewhere. Home for the
/// staged binary + logs (ADR 0017), the relaunch sentinel, and the FE control
/// channel (ADR 0019).
fn sot_state_dir() -> Option<std::path::PathBuf> {
    let dir = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| std::path::PathBuf::from(h).join(".local").join("state"))
            })
    }?;
    Some(dir.join("sot"))
}

/// Path of the self-relaunch sentinel file (ADR 0017). The build-and-relaunch
/// helper (`scripts/relaunch-sot.ps1`) creates this after a successful
/// `cargo build`; the watcher thread notices it and triggers an exit-75
/// respawn.
fn relaunch_sentinel_path() -> Option<std::path::PathBuf> {
    sot_state_dir().map(|d| d.join("relaunch.request"))
}

/// Directory of pending FE-control command files (ADR 0019). An in-terminal
/// agent (or the user) drops one JSON object per file here; the FE's command
/// watcher reads, deletes, and enqueues each for main-thread dispatch.
fn fe_commands_dir() -> Option<std::path::PathBuf> {
    sot_state_dir().map(|d| d.join("fe-commands"))
}

/// Path of the FE state-readback file (ADR 0019). The FE rewrites it
/// (atomic temp+rename) whenever the observable state changes, so an
/// in-terminal agent can poll it instead of screenshotting.
fn fe_state_path() -> Option<std::path::PathBuf> {
    sot_state_dir().map(|d| d.join("fe-state.json"))
}

/// Path of the agent-relay inbox (`<state-dir>/fe-inbox.jsonl`). The daemon
/// pushes `agent.message` evt frames over the SSH-forwarded socket; the FE
/// appends each as one JSON line here so the in-terminal agent on this
/// machine receives cross-machine messages instantly instead of polling the
/// git bus. One object per line: `{"from":..,"to":..,"text":..,"ts":..}`.
fn fe_inbox_path() -> Option<std::path::PathBuf> {
    sot_state_dir().map(|d| d.join("fe-inbox.jsonl"))
}

/// A parsed `sot_ui` nav command (Item 2 — a session driving this FE's
/// nav). Carried in an `agent.message` text payload; intercepted before the
/// inbox append so it never renders as chat. v1 is `nav.preview` only.
#[derive(Debug, Clone, PartialEq)]
struct NavEnvelope {
    /// Workspace slug the path is relative to — the FE acts only when this
    /// matches its currently-active workspace (the gate).
    workspace: String,
    /// Workspace-relative file path to preview (→ `files:<path>` node id).
    path: String,
}

/// Parse an `agent.message` text payload as a v1 `sot_ui` nav.preview
/// envelope. Exact shape (anything else → `None`, so the caller falls
/// through to ordinary inbox/chat rendering):
///   {"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"<slug>",
///                 "mode":"files","path":"<ws-rel>"}}
/// Pure + total so it's unit-testable and can't panic in the event drain.
/// `mode` is accepted-and-ignored in v1 (only files-mode preview exists);
/// `cmd` leaves room to grow. The path is taken verbatim — the emitter
/// already relativized any absolute path against the workspace root.
fn parse_nav_envelope(text: &str) -> Option<NavEnvelope> {
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let ui = v.get("sot_ui")?;
    if ui.get("v").and_then(|x| x.as_i64()) != Some(1) {
        return None;
    }
    if ui.get("cmd").and_then(|x| x.as_str()) != Some("nav.preview") {
        return None;
    }
    let workspace = ui.get("workspace").and_then(|x| x.as_str())?.to_string();
    let path = ui.get("path").and_then(|x| x.as_str())?.to_string();
    if workspace.is_empty() || path.is_empty() {
        return None;
    }
    Some(NavEnvelope { workspace, path })
}

/// This FE's deterministic sot-comm handle (ADR 0025 target filter). Mirrors
/// `state_persistence::state_path`'s hostname logic exactly: `$HOSTNAME` (Linux)
/// else `$COMPUTERNAME` (Windows) else "unknown", lowercased, as
/// `win-fe-<host>`. The daemon scopes an `FE_COMMAND`'s `target` to one FE by
/// this handle; we self-filter against it.
fn self_comm_handle() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    format!("win-fe-{}", host.to_lowercase())
}

/// Pure routing decision for an `FE_COMMAND` evt (ADR 0025): apply the target
/// filter, then map `(cmd, args)` to the `FeCommand` the dispatch sink runs.
/// `None` means "ignore" — for a bad target (scoped to another FE), an unknown
/// `cmd`, or a `cmd` missing a required arg. Pure + total so it's unit-testable;
/// the dispatch side effects (which need a `State`) are exercised separately.
///
/// Target filter: `None` → act (the badge floor; every FE acts). `Some(h)` →
/// act only when `h == self_handle` (force-show scoped to this FE); otherwise
/// ignore. The caller treats a `Some(h) == self_handle` match as force-show
/// eligible (`urgent` carried through to `dispatch_fe_command`, which still
/// gates on `fe_is_idle`).
fn route_fe_command(evt: &sot_protocol::ops::FeCommandEvt, self_handle: &str) -> Option<FeCommand> {
    // Target filter first — cheapest reject, and a mis-targeted command
    // shouldn't even be parsed.
    if let Some(h) = evt.target.as_deref() {
        if h != self_handle {
            return None;
        }
    }
    let args = &evt.args;
    let str_arg = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let bool_arg = |key: &str| args.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
    // Force-show is DIRECTED-only. A broadcast (target None = the badge floor)
    // must NEVER force-show — an `urgent` broadcast would otherwise yank EVERY
    // idle FE's view at once. `urgent` is honoured only when the command is
    // targeted to THIS FE (target Some == self, having passed the filter
    // above); a broadcast collapses to the non-disruptive badge regardless of
    // the `urgent` arg.
    let directed = evt.target.is_some();
    match evt.cmd.as_str() {
        "preview" => {
            let workspace = str_arg("workspace")?;
            let path = str_arg("path")?;
            Some(FeCommand::Preview {
                workspace,
                path,
                urgent: bool_arg("urgent") && directed,
            })
        }
        "reveal" => {
            let workspace = str_arg("workspace")?;
            let path = str_arg("path")?;
            Some(FeCommand::Reveal {
                workspace,
                path,
                urgent: bool_arg("urgent") && directed,
            })
        }
        "goto_workspace" => {
            let workspace = str_arg("workspace")?;
            Some(FeCommand::Workspace {
                slug: Some(workspace),
                boot: bool_arg("boot"),
            })
        }
        "goto_mode" => {
            let mode = str_arg("mode")?;
            Some(FeCommand::Mode { mode })
        }
        "notify" => {
            let text = str_arg("text")?;
            Some(FeCommand::Notify {
                text,
                level: str_arg("level"),
            })
        }
        "open_url" => {
            let url = str_arg("url")?;
            // Scheme allowlist: a BE-relayed command must not be able to
            // launch arbitrary local handlers (file:, javascript:, custom
            // protocol hijacks). Browsers own http/https; nothing else.
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                tracing::warn!(%url, "fe-command open_url: non-http(s) scheme refused");
                return None;
            }
            Some(FeCommand::OpenUrl { url })
        }
        _ => None,
    }
}

/// Append one `agent.message` evt payload to `fe-inbox.jsonl` (creating the
/// file/dir if needed). Re-serializes the payload verbatim as a single line.
/// Non-fatal on IO error: log and continue — a missed relay must not take
/// down the frontend.
fn append_agent_message(payload: &serde_json::Value) {
    let Some(path) = fe_inbox_path() else {
        tracing::warn!("agent.message: no state dir; dropping");
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(error = %e, "agent.message: create state dir failed");
            return;
        }
    }
    let mut line = match serde_json::to_string(payload) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "agent.message: serialize failed");
            return;
        }
    };
    line.push('\n');
    use std::io::Write;
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!(error = %e, ?path, "agent.message: append failed");
            }
        }
        Err(e) => tracing::warn!(error = %e, ?path, "agent.message: open inbox failed"),
    }
}

/// A control command raised through the FE command-file channel (ADR 0019).
/// One JSON object per file under `fe-commands/`, internally tagged by `cmd`.
/// The watcher parses into this enum and the main thread dispatches each
/// through the same methods the keybinds use. New variants land here as later
/// ADR-0019 commits add command groups (reload_keybindings, notify, mode, nav).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum FeCommand {
    /// Switch to a named workspace by slug. A null/absent/"default" slug
    /// resolves to the daemon-default workspace (`active_workspace_id = None`).
    Workspace {
        #[serde(default)]
        slug: Option<String>,
        /// `--boot`: before switching, seed the target workspace's autostart
        /// flag so attach_session_to_bl boots ccb on attach — a scriptable
        /// spawn->goto->boot that's unconditional of what workspace.list has
        /// reported yet (sidesteps the registry-flag timing). Wire arg "boot".
        #[serde(default)]
        boot: bool,
    },
    /// Cycle the active workspace by `dir` (+1 next, -1 prev); wraps. Defaults
    /// to +1 so `{"cmd":"cycle_ws"}` means "next".
    CycleWs {
        #[serde(default = "fe_cmd_default_dir")]
        dir: i32,
    },
    /// Re-read the layered keybindings config live — for after an
    /// in-terminal agent edits `.sot/keybindings.toml` (or another layer).
    ReloadKeybindings,
    /// Surface a text message in the chrome status line. `level` is advisory
    /// (info|warn|error) and rendered uniformly for now.
    Notify {
        text: String,
        #[serde(default)]
        level: Option<String>,
    },
    /// Open an arbitrary web URL in THIS machine's OS browser (maintainer note,
    /// 2026-07-05: "not port forward, FE opens"). http/https only —
    /// enforced at route time so a hostile scheme never reaches dispatch.
    OpenUrl { url: String },
    /// Switch nav mode: "files" | "modules" | "sessions" | "hosts".
    Mode { mode: String },
    /// Drive the nav cursor/tree: "up" | "down" | "expand" | "collapse" |
    /// "pin". Cursor *activation* (keyboard Enter, with its Sessions/Hosts
    /// side effects) stays keyboard-only — the `workspace` command already
    /// covers the session-switch case.
    Nav { action: String },
    /// ADR 0022: capture the visible image-preview ROI and send it to the LLM
    /// pane. Lets the in-pane agent trigger a capture (`{"cmd":"capture_roi"}`)
    /// without the `C` hotkey — the "look at what I'm zoomed into" pull path.
    /// No-op (status hint) when the preview isn't a croppable image.
    CaptureRoi,
    /// ADR 0025 imperative preview: show `path` (workspace-relative) in
    /// `workspace`'s Files-mode preview. `urgent` requests force-show (switch +
    /// show now); without it — or when the FE isn't idle — this degrades to the
    /// badge floor (`mark_pending_nav`), the non-disruptive default. Carried as
    /// an `FE_COMMAND` evt's `{cmd:"preview", args:{workspace, path, urgent?}}`;
    /// also constructible from the ADR-0019 file channel for parity.
    Preview {
        workspace: String,
        path: String,
        #[serde(default)]
        urgent: bool,
    },
    /// ADR 0025 imperative reveal — v1 is identical to `Preview` (badge floor /
    /// force-show + on-switch preview). Deep tree-expand-and-select of the
    /// target row is a documented v1.1 follow-up; until then `reveal` reuses the
    /// preview path so a result still reaches the user.
    Reveal {
        workspace: String,
        path: String,
        #[serde(default)]
        urgent: bool,
    },
}

fn fe_cmd_default_dir() -> i32 {
    1
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let evt_rx = match self.evt_rx.take() {
            Some(rx) => rx,
            None => {
                tracing::error!("evt_rx already consumed");
                event_loop.exit();
                return;
            }
        };
        match State::new(event_loop, evt_rx, &self.cli, self.req_tx.clone()) {
            Ok(state) => {
                // Spawn transport once the window exists, since the task
                // needs an Arc<Window> to call request_redraw on incoming
                // frames. Either or both of `socket`/`tcp` may be set; the
                // transport task picks pipe first and falls back to TCP on
                // connect failure.
                let any_endpoint = self.cli.socket.is_some() || self.cli.tcp.is_some();
                if let (Some(rt), true, Some(evt_tx), Some(req_rx)) = (
                    self.rt.as_ref(),
                    any_endpoint,
                    self.evt_tx.take(),
                    self.req_rx.take(),
                ) {
                    let config = crate::transport::TransportConfig {
                        pipe: self.cli.socket.clone(),
                        tcp: self.cli.tcp.clone(),
                        token: self.cli.token.clone(),
                    };
                    crate::transport::spawn(
                        rt,
                        config,
                        evt_tx,
                        req_rx,
                        state.window.clone(),
                        state.reconnect_now.clone(),
                    );
                }
                // If `--start-mode modules` was set, queue a project.scan
                // request now so the chrome's initial render is the
                // unified Modules/Types tree. Mostly for `--capture`,
                // where we can't inject `m` mid-run.
                if state.mode == Mode::Modules {
                    if let Err(e) = state.req_tx.send(OutgoingReq::ProjectScan {
                        workspace_id: state.active_workspace_id.clone(),
                    }) {
                        tracing::warn!(error = %e, "drop initial project.scan request");
                    }
                }
                state.window.request_redraw();
                // Self-relaunch watcher (ADR 0017): poll for the sentinel
                // file that the build-and-relaunch helper drops. On first
                // sight, flag it and wake the window; `window_event` then
                // exits with code 75 so the supervisor respawns us. A
                // background thread (not the control-flow timer) keeps the
                // interactive `Wait` power profile intact.
                if let Some(sentinel) = relaunch_sentinel_path().filter(|_| !state.ephemeral) {
                    let flag = state.relaunch_flag.clone();
                    let waker = state.window.clone();
                    if let Err(e) = std::thread::Builder::new()
                        .name("sot-relaunch-watch".to_string())
                        .spawn(move || loop {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            if sentinel.exists() {
                                let _ = std::fs::remove_file(&sentinel);
                                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                                waker.request_redraw();
                                break;
                            }
                        })
                    {
                        tracing::warn!(error = %e, "failed to spawn relaunch watcher");
                    }
                }
                // FE control-command watcher (ADR 0019): poll the fe-commands
                // dir for JSON command files dropped by an in-terminal agent
                // or the user. Parse + enqueue each, delete the file, and wake
                // the window so `window_event` drains the queue on the main
                // thread. Persistent (no break) — unlike the one-shot relaunch
                // watcher above.
                // Both watchers DELETE what they read, so a harness FE would
                // eat the primary FE's relaunch sentinel / control commands —
                // ephemeral instances don't arm them (B8).
                if let Some(cmd_dir) = fe_commands_dir().filter(|_| !state.ephemeral) {
                    let _ = std::fs::create_dir_all(&cmd_dir);
                    let queue = state.fe_commands.clone();
                    let waker = state.window.clone();
                    if let Err(e) = std::thread::Builder::new()
                        .name("sot-fe-command-watch".to_string())
                        .spawn(move || loop {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            let entries = match std::fs::read_dir(&cmd_dir) {
                                Ok(e) => e,
                                Err(_) => continue,
                            };
                            // Sort by filename so a burst is processed roughly
                            // FIFO (writers can prefix a counter/timestamp).
                            let mut paths: Vec<std::path::PathBuf> = entries
                                .filter_map(|e| e.ok().map(|e| e.path()))
                                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
                                .collect();
                            paths.sort();
                            let mut woke = false;
                            for path in paths {
                                let bytes = match std::fs::read(&path) {
                                    Ok(b) => b,
                                    Err(_) => continue,
                                };
                                // Delete first so a malformed file can't loop
                                // forever on the next tick.
                                let _ = std::fs::remove_file(&path);
                                match serde_json::from_slice::<FeCommand>(&bytes) {
                                    Ok(cmd) => {
                                        if let Ok(mut q) = queue.lock() {
                                            q.push_back(cmd);
                                            woke = true;
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            path = %path.display(),
                                            "bad fe-command file dropped"
                                        );
                                    }
                                }
                            }
                            if woke {
                                waker.request_redraw();
                            }
                        })
                    {
                        tracing::warn!(error = %e, "failed to spawn fe-command watcher");
                    }
                }
                self.state = Some(state);
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to bring up wgpu surface");
                event_loop.exit();
            }
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        // Self-relaunch (ADR 0017): the watcher thread set this when the
        // sentinel appeared. Persist geometry, then exit 75 — the supervisor
        // restages the freshly-built binary and respawns us with
        // `--relaunched`. Abrupt exit is fine: state is saved on events, and
        // the OS reclaims the window/GPU surface.
        if state
            .relaunch_flag
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            tracing::info!("relaunch requested; exiting 75 for supervisor respawn");
            state.persist_resume_state();
            // We currently own the OS foreground, so we're allowed to hand the
            // foreground right to the about-to-spawn replacement. ASFW_ANY lifts
            // the Win32 foreground lock for the next SetForegroundWindow from
            // any process — which the relaunched FE issues on its first paint
            // (force_os_foreground). Without this the new process is blocked
            // and only flashes the taskbar. ADR 0017.
            #[cfg(windows)]
            allow_next_foreground();
            std::process::exit(75);
        }
        // FE control commands (ADR 0019): drain whatever the watcher enqueued
        // and dispatch on the main thread — same code paths as the keybinds.
        // Cheap no-op when the queue is empty.
        state.drain_fe_commands();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.resize(size);
                state.persist_resume_state();
                state.window.request_redraw();
            }
            WindowEvent::Moved(_) => {
                state.persist_resume_state();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                state.resize(state.window.inner_size());
                state.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(mods) => {
                // Cache the active modifier state. winit 0.30 doesn't ride
                // modifiers on KeyEvent, so the KeyboardInput arm consults
                // this for Ctrl+Arrow pane navigation.
                self.modifiers = mods.state();
            }
            WindowEvent::Focused(focused) => {
                // winit can drop a Ctrl/Shift/Alt release event when the
                // window loses focus mid-keystroke (alt-tab, lock
                // screen, etc.), which leaves `self.modifiers` stuck.
                // The next arrow key then triggers Ctrl+Arrow pane move
                // instead of tree nav, and the user reasonably reports
                // "nav broken". Clear the modifier cache on every
                // focus transition so we re-learn from the next
                // ModifiersChanged event.
                if !focused {
                    self.modifiers = winit::keyboard::ModifiersState::empty();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.cursor_px = (position.x as f32, position.y as f32);
                // Extend the LLM-pane selection while the user is dragging.
                // Drag uses non-strict cell mapping so the user can pull
                // outside the pane to extend selection to the edge.
                if state.llm_drag_active {
                    if let Some(end) = state.llm_cell_at_px(state.cursor_px, false) {
                        if let Some((start, _)) = state.llm_selection {
                            state.llm_selection = Some((start, end));
                            state.window.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: btn_state,
                button,
                ..
            } => {
                if button == MouseButton::Left {
                    match btn_state {
                        ElementState::Pressed => {
                            // Mouse-down inside the LLM pane starts a new
                            // selection at the clicked cell and grabs
                            // focus so subsequent keys (Ctrl+Shift+C copy)
                            // land in the LLM arm. Outside the pane: clear
                            // any existing selection so a click elsewhere
                            // dismisses the highlight.
                            if let Some(cell) = state.llm_cell_at_px(state.cursor_px, true) {
                                state.focus = PaneFocus::Llm;
                                state.llm_selection = Some((cell, cell));
                                state.llm_drag_active = true;
                                state.window.request_redraw();
                            } else if state.llm_selection.is_some() {
                                state.llm_selection = None;
                                state.window.request_redraw();
                            }
                        }
                        ElementState::Released => {
                            // Mouse-up just ends the drag — selection
                            // stays painted so the user has time to hit
                            // Ctrl+Shift+C.
                            state.llm_drag_active = false;
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Convert the platform delta into fractional rows, then
                // accumulate. Precision touchpads emit small sub-row
                // pixel deltas that would otherwise truncate to zero
                // and feel dead. Standard wheel ticks (LineDelta y=1)
                // step three rows, matching the common TUI cadence.
                let (delta_rows, raw_px_y, raw_px_x): (f32, f32, f32) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (y * 3.0, y * state.cell_h * 3.0, x * state.cell_w * 3.0)
                    }
                    MouseScrollDelta::PixelDelta(pos) => {
                        (pos.y as f32 / state.cell_h, pos.y as f32, pos.x as f32)
                    }
                };
                // Shift+wheel-Y *or* a horizontal-axis wheel event in
                // Preview focus → wide-table horizontal scroll. Take
                // it before the vertical accumulator runs so a held
                // Shift doesn't also walk preview_scroll. Sign: wheel
                // up shifts the table left (reveal more right-side
                // content). Clamp happens in redraw.
                let shift = self.modifiers.shift_key();
                if state.focus == PaneFocus::Preview {
                    let h_px = if shift && raw_px_y.abs() > 0.0 {
                        -raw_px_y
                    } else if raw_px_x.abs() > 0.0 {
                        raw_px_x
                    } else {
                        0.0
                    };
                    if h_px.abs() > 0.0 {
                        state.md_table_scroll_px = (state.md_table_scroll_px + h_px).max(0.0);
                        state.window.request_redraw();
                        return;
                    }
                }
                state.wheel_residue_y += delta_rows;
                let rows_above = state.wheel_residue_y.trunc() as i32;
                state.wheel_residue_y -= rows_above as f32;
                tracing::info!(
                    delta_rows,
                    rows_above,
                    residue = state.wheel_residue_y,
                    ?state.focus,
                    "wheel"
                );
                if rows_above == 0 {
                    return;
                }
                // Preview's scroll origin is the top of the doc, REPL
                // and LLM's are the tail — sign flip lives in each
                // pane's apply step so a single positive `rows_above`
                // feels like "show content above" everywhere.
                match state.focus {
                    PaneFocus::Repl if state.drawer == DrawerContent::Terminal => {
                        // The drawer is the local terminal. If the running app
                        // grabbed the mouse (vim/less/htop), forward the wheel
                        // as an SGR sequence so it scrolls its own view; else
                        // walk our vt100 scrollback ring (set_scrollback is
                        // applied in redraw). Sign: positive rows_above = up =
                        // older = larger offset, matching the REPL pane.
                        let mouse_on = state
                            .local_term
                            .as_ref()
                            .map(|t| t.mouse_tracking_on())
                            .unwrap_or(false);
                        if mouse_on {
                            if let Some(t) = state.local_term.as_mut() {
                                let button = if rows_above > 0 { 64 } else { 65 };
                                let n = rows_above.unsigned_abs().min(8);
                                for _ in 0..n {
                                    t.send_input(format!("\x1b[<{button};1;1M").as_bytes());
                                }
                            }
                        } else {
                            let new = (state.term_scroll as i32 + rows_above).max(0);
                            state.term_scroll = new as u16;
                        }
                        state.window.request_redraw();
                    }
                    PaneFocus::Repl => {
                        let new = (state.repl_scroll as i32 + rows_above).max(0);
                        state.repl_scroll = new as u16;
                        state.window.request_redraw();
                    }
                    PaneFocus::Preview => {
                        let new = (state.preview_scroll as i32 - rows_above).max(0);
                        state.preview_scroll = new as u16;
                        state.window.request_redraw();
                    }
                    PaneFocus::Llm => {
                        // Tmux owns scrollback; our vt100-ctt ring stays
                        // empty because tmux uses cursor-positioned
                        // redraws. Forward wheel events to the pty as
                        // xterm SGR mouse-tracking sequences and let
                        // tmux scroll its own ring. Needs `set -g mouse
                        // on` inside the tmux session.
                        //
                        // Throttled to one fire per ~120ms because each
                        // SGR triggers a tmux pane repaint round-tripped
                        // through SSH; without throttling, touchpad
                        // events at 60Hz pile up and scroll visibly
                        // lags behind the wheel. Excess events get
                        // dropped on the floor (stale residue cleared)
                        // — "fast scrolling" then translates to "max
                        // ~8 scroll lines/sec", which is plenty for
                        // human reading pace and stays in sync with
                        // the wheel.
                        let now = std::time::Instant::now();
                        let elapsed = state
                            .last_pty_wheel_at
                            .map(|t| now.duration_since(t))
                            .unwrap_or(std::time::Duration::MAX);
                        state.wheel_residue_y = 0.0;
                        if elapsed < std::time::Duration::from_millis(120) {
                            return;
                        }
                        state.last_pty_wheel_at = Some(now);
                        let button = if rows_above > 0 { 64 } else { 65 };
                        let seq = format!("\x1b[<{};1;1M", button);
                        if let Err(e) = state.req_tx.send(OutgoingReq::PtyWrite {
                            bytes: seq.into_bytes(),
                        }) {
                            tracing::warn!(error = %e, "drop pty.write (wheel) — channel closed");
                        }
                    }
                    PaneFocus::NavTree => {
                        // Nav is cursor-driven; wheel-scroll without
                        // moving the cursor would desync the two. No-op
                        // until there's a richer story for it.
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // Frame-rate cap: if the previous frame finished less than
                // FRAME_BUDGET ago, defer. `about_to_wait` reschedules at
                // the next frame boundary, so this draw isn't dropped —
                // just collapsed with whatever else arrives in the
                // intervening few ms. Capture mode bypasses the cap so
                // frame_counter ticks up to CAPTURE_FRAME without delay.
                let throttled = state.capture_path.is_none()
                    && state
                        .last_frame_at
                        .map(|t| t.elapsed() < FRAME_BUDGET)
                        .unwrap_or(false);
                if throttled {
                    state.dirty = true;
                } else {
                    state.dirty = false;
                    if let Err(e) = state.redraw() {
                        tracing::error!(error = %e, "redraw failed");
                    }
                    // ADR 0019: refresh fe-state.json if the observable state
                    // changed this frame (cheap signature no-op otherwise).
                    state.maybe_write_fe_state();
                    // Portable focus-on-launch: now that the window is shown
                    // and has painted once, attempt to take focus + raise.
                    // Window managers that refuse focus-stealing (Windows
                    // foreground-lock, macOS) get the OS-sanctioned fallback
                    // of a user-attention request. One-shot. ADR 0017.
                    if state.focus_on_first_frame {
                        state.focus_on_first_frame = false;
                        state.window.focus_window();
                        // Windows blocks SetForegroundWindow for a freshly
                        // spawned process (foreground lock), so a relaunched
                        // FE lands behind. force_os_foreground escalates
                        // (attach-thread → topmost-toggle → minimize/restore)
                        // and reports whether we actually took the foreground.
                        // Only fall back to a taskbar flash if it didn't.
                        // ADR 0017.
                        #[cfg(windows)]
                        let got_foreground = force_os_foreground(&state.window);
                        #[cfg(not(windows))]
                        let got_foreground = false;
                        if !got_foreground {
                            state.window.request_user_attention(Some(
                                winit::window::UserAttentionType::Critical,
                            ));
                        }
                    }
                    if state.should_exit {
                        event_loop.exit();
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                if is_synthetic {
                    // Focus-driven synthetic key events (Alt etc. on focus
                    // change) don't represent user intent; ignore.
                    return;
                }
                // Allow repeat for arrow keys (Up/Down hold-to-scroll feels
                // wrong without it) but not for action keys.
                if event.state != ElementState::Pressed {
                    return;
                }
                // Snapshot-and-clear the destroy arm. The D handler
                // re-arms on first press; any other key (cursor move,
                // mode switch, etc.) silently clears it. Same pattern
                // as the now-retired exit-confirm.
                let was_destroy_pending = state.pending_destroy_target.clone();
                state.pending_destroy_target = None;
                let label = key_label(&event.logical_key);
                let ctrl = self.modifiers.control_key();
                let alt = self.modifiers.alt_key();
                let shift = self.modifiers.shift_key();
                let super_ = self.modifiers.super_key();
                tracing::info!(
                    ?event.logical_key,
                    label = %label,
                    repeat = event.repeat,
                    ctrl,
                    "key pressed"
                );
                // F5: manual reconnect trigger — collapses the
                // transport's current backoff sleep and retries
                // immediately. Works from any focus, no modifier, so
                // it's there when wifi comes back and the user
                // doesn't want to wait the up-to-5s backoff cap.
                if !event.repeat
                    && state.bindings.matches(
                        Action::Reconnect,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    )
                {
                    state.reconnect_now.notify_one();
                    state.last_key = Some(label);
                    state.window.request_redraw();
                    return;
                }
                // F5 handled above (manual reconnect). F11: borderless
                // fullscreen toggle — standard cross-platform key for
                // this, no modifier, no conflict with anything we bind
                // (Ctrl+F clashes with readline forward-char in the
                // LLM shell, so we avoid it).
                if !event.repeat
                    && state.bindings.matches(
                        Action::ToggleFullscreen,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    )
                {
                    let new_fs = if state.window.fullscreen().is_some() {
                        None
                    } else {
                        Some(Fullscreen::Borderless(None))
                    };
                    state.window.set_fullscreen(new_fs);
                    state.last_key = Some(label);
                    state.window.request_redraw();
                    return;
                }
                // Ctrl+= / Ctrl+- / Ctrl+0: global font scale. Intercepted
                // first so they reach this handler even in LLM focus
                // (where most other Ctrl+letter bytes are forwarded to
                // the pty). +0.1 / -0.1 per press, reset to 1.0 on
                // Ctrl+0; clamped to [0.5, 3.0].
                // Font scale is keymap-driven (font.scale_up / _down / _reset).
                // Intercepted before per-pane dispatch so it works even in LLM
                // focus (where most Ctrl+letter bytes forward to the pty).
                if !event.repeat {
                    if state.bindings.matches(
                        Action::FontScaleUp,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        state.apply_text_scale(state.text_scale_mult + 0.1);
                        state.persist_resume_state();
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    if state.bindings.matches(
                        Action::FontScaleDown,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        state.apply_text_scale(state.text_scale_mult - 0.1);
                        state.persist_resume_state();
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    if state.bindings.matches(
                        Action::FontScaleReset,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        state.apply_text_scale(1.0);
                        state.persist_resume_state();
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                }
                // Ctrl+Arrow: spatial pane focus move (4-way grid). The
                // arrow-only case stays per-pane (tree nav / no-op),
                // unmodified.
                if !event.repeat {
                    // Spatial pane focus is keymap-driven (focus.pane_*); the
                    // default Ctrl+Arrow chords keep it disjoint from plain
                    // arrows (per-pane nav) and Shift+Arrow (workspace cycle).
                    let dir = if state.bindings.matches(
                        Action::FocusPaneRight,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(SpatialDir::Right)
                    } else if state.bindings.matches(
                        Action::FocusPaneLeft,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(SpatialDir::Left)
                    } else if state.bindings.matches(
                        Action::FocusPaneUp,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(SpatialDir::Up)
                    } else if state.bindings.matches(
                        Action::FocusPaneDown,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(SpatialDir::Down)
                    } else {
                        None
                    };
                    if let Some(dir) = dir {
                        let next = state.focus.move_in(dir);
                        // When the REPL drawer is closed, don't let
                        // spatial Down land focus on the invisible Repl
                        // slot. User must explicitly
                        // summon the drawer (Ctrl+J) before it can take
                        // focus.
                        if !(next == PaneFocus::Repl && !state.drawer.is_open()) {
                            state.focus = next;
                        }
                        state.last_key = Some(format!("Ctrl+{label}"));
                        state.window.request_redraw();
                        return;
                    }
                }
                // Tab is intentionally NOT a focus switcher: it would
                // steal shell/REPL completion in the terminal panes.
                // Focus changes go through Ctrl+Arrow; Tab falls through
                // to the focused pane (forwarded to the pty as `\t`).
                // Shift+ArrowRight / Shift+ArrowLeft cycles the active
                // workspace forward / backward (ADR 0014 D7). Intercepted
                // globally — including LLM focus — so the user can flip
                // workspaces mid-shell-session without re-focusing the nav
                // pane. No-op when only the default workspace is registered.
                // `!event.repeat` so a held keypress doesn't blast through
                // every workspace; one switch per press. `!ctrl && !alt`
                // keeps it disjoint from Ctrl+Arrow (spatial pane move);
                // plain (unmodified) arrows still fall through to per-pane
                // nav. Trade-off: Shift+Arrow no longer reaches the pty in
                // the LLM / terminal panes (it previously forwarded a bare
                // arrow there).
                // Workspace cycle is keymap-driven (workspace.cycle_next /
                // workspace.cycle_prev). Suppressed in edit mode so it doesn't
                // hijack arrows in the editor; the default Shift+Arrow chords
                // keep it disjoint from Ctrl+Arrow (pane focus) above.
                if !event.repeat && state.edit_state.is_none() {
                    if state.bindings.matches(
                        Action::WorkspaceCycleNext,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        state.cycle_workspace(1);
                        state.last_key = Some(label);
                        return;
                    }
                    if state.bindings.matches(
                        Action::WorkspaceCyclePrev,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        state.cycle_workspace(-1);
                        state.last_key = Some(label);
                        return;
                    }
                }
                // Maximise / restore the focused pane via Alt+= (maximise)
                // and Esc (restore) — defaults, overridable in the
                // keybindings file. The visible pane follows `focus`, so
                // Ctrl+Arrow while maximised swaps which pane is on screen —
                // what "I'm zoomed in but want to peek at another pane" wants.
                // Maximise is intercepted globally including LLM focus (user
                // picked pane-management consistency over forwarding Alt+= to
                // the shell). Restore is gated on `state.maximized` so Esc
                // only un-maximises when a pane is actually maximised —
                // otherwise Esc falls through to the pty / edit mode / etc.
                if !event.repeat {
                    if state.bindings.matches(
                        Action::MaximizePane,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        state.maximized = true;
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    // `!help_open` so the help overlay's Esc wins when it's up
                    // (it's modal); un-maximize resumes once the overlay closes.
                    if state.maximized
                        && !state.help_open
                        && state.bindings.matches(
                            Action::RestoreLayout,
                            &event.logical_key,
                            ctrl,
                            alt,
                            shift,
                        )
                    {
                        state.maximized = false;
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    // `?` toggles the keybindings help overlay. Gated to the
                    // two reading panes + no edit modal, so a literal `?`
                    // typed into the REPL / terminal / LLM / concept editor is
                    // never swallowed. Esc closes it from here too.
                    if !ctrl
                        && !alt
                        && state.edit_state.is_none()
                        && matches!(state.focus, PaneFocus::NavTree | PaneFocus::Preview)
                    {
                        if state.bindings.matches(
                            Action::ToggleHelp,
                            &event.logical_key,
                            ctrl,
                            alt,
                            shift,
                        ) {
                            state.help_open = !state.help_open;
                            if state.help_open {
                                state.rebuild_help_overlay();
                            }
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                    }
                    if state.help_open {
                        if let Key::Named(NamedKey::Escape) = &event.logical_key {
                            state.help_open = false;
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                    }
                    // Ctrl+Shift+S: whole-window selfie to a timestamped PNG.
                    // Handled here in the global-chord region so it fires from
                    // ANY pane — including the terminal/REPL drawers, before
                    // keystrokes route into a pty. The readback runs in the
                    // render loop on the next frame (request_redraw below).
                    if state
                        .bindings
                        .matches(Action::Selfie, &event.logical_key, ctrl, alt, shift)
                    {
                        state.selfie_pending = Some(selfie_path());
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                    // Ctrl+J: toggle the REPL drawer (ADR 0014 layout
                    // rework). VS Code's panel-toggle convention; reads
                    // intuitively as "show me the bottom panel". When
                    // the drawer opens, focus moves into it so the user
                    // can immediately type. When it closes, focus
                    // bounces back to NavTree (the most useful default
                    // landing pane).
                    // Ctrl+J (Repl) and Ctrl+T (Terminal) are symmetric:
                    // each opens its own drawer content, swaps to it if the
                    // other is showing, and closes if its own is already
                    // showing. Both share the `PaneFocus::Repl` drawer slot;
                    // `state.drawer` decides which content renders (and, per
                    // G4, where keystrokes route). When the drawer is open
                    // focus moves into it; when it closes from the drawer,
                    // focus bounces back to NavTree.
                    // Drawer toggles are keymap-driven (.sot/keybindings.toml:
                    // drawer.repl / drawer.terminal / drawer.monitor) so the
                    // chords reconfigure without a recompile. Defaults Ctrl+j /
                    // Ctrl+t / Ctrl+m preserve the prior behaviour.
                    let drawer_key = if state.bindings.matches(
                        Action::ToggleReplDrawer,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(DrawerContent::Repl)
                    } else if state.bindings.matches(
                        Action::ToggleTerminalDrawer,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(DrawerContent::Terminal)
                    } else if state.bindings.matches(
                        Action::ToggleMonitorDrawer,
                        &event.logical_key,
                        ctrl,
                        alt,
                        shift,
                    ) {
                        Some(DrawerContent::Monitor)
                    } else {
                        None
                    };
                    if let Some(slot) = drawer_key {
                        state.drawer = state.drawer.toggle(slot);
                        if state.drawer.is_open() {
                            state.focus = PaneFocus::Repl;
                        } else if state.focus == PaneFocus::Repl {
                            state.focus = PaneFocus::NavTree;
                        }
                        // Monitor drawer subscribe/unsubscribe lifecycle (ADR
                        // 0020): subscribe + prefill on open, unsubscribe on
                        // close. Backend sampling is always-on; this just gates
                        // this connection's live stream to when the drawer is up.
                        if state.drawer == DrawerContent::Monitor && !state.monitor_view.subscribed
                        {
                            let _ = state
                                .req_tx
                                .send(crate::transport::OutgoingReq::MonitorSubscribe);
                            let _ =
                                state
                                    .req_tx
                                    .send(crate::transport::OutgoingReq::MonitorHistory {
                                        window_s: 300.0,
                                        points: 300,
                                        until: None,
                                        host: None,
                                    });
                            state.monitor_view.subscribed = true;
                            state.monitor_dirty = true;
                        } else if state.drawer != DrawerContent::Monitor
                            && state.monitor_view.subscribed
                        {
                            let _ = state
                                .req_tx
                                .send(crate::transport::OutgoingReq::MonitorUnsubscribe);
                            state.monitor_view.subscribed = false;
                        }
                        state.last_key = Some(label);
                        state.window.request_redraw();
                        return;
                    }
                }
                // Alt+Up / Alt+Down: fine-grained one-row scroll in the
                // focused pane. Shared across REPL and Preview here so
                // the rule reads in one place. NavTree is cursor-driven
                // (manual scroll would desync) and LLM passes alt+arrow
                // through to the pty so tmux/shell keep alt-keybinds —
                // both fall through to the per-pane match below.
                if alt && !ctrl {
                    let row_step: i32 = 1;
                    match (state.focus, &event.logical_key) {
                        (PaneFocus::Repl, Key::Named(NamedKey::ArrowUp)) => {
                            state.repl_scroll = state.repl_scroll.saturating_add(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        (PaneFocus::Repl, Key::Named(NamedKey::ArrowDown)) => {
                            state.repl_scroll = state.repl_scroll.saturating_sub(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        (PaneFocus::Preview, Key::Named(NamedKey::ArrowUp)) => {
                            state.preview_scroll =
                                state.preview_scroll.saturating_sub(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        (PaneFocus::Preview, Key::Named(NamedKey::ArrowDown)) => {
                            state.preview_scroll =
                                state.preview_scroll.saturating_add(row_step as u16);
                            state.window.request_redraw();
                            return;
                        }
                        _ => {}
                    }
                }
                // Wide-table horizontal scroll: h/l step the shared
                // `md_table_scroll_px` by one body-em (≈ the width of
                // one monospace cell). `0` resets to scroll-left.
                // Plain keys (no modifier) so the binding is one-handed
                // and fast; ignored unless the focus is Preview so the
                // letters stay typeable in LLM/REPL. Only does
                // anything when the current doc actually contains a
                // table wider than the preview pane; otherwise the
                // redraw clamp keeps scroll at 0.
                if state.focus == PaneFocus::Preview
                    && !ctrl
                    && !alt
                    && !super_
                    && state.preview_png.is_none()
                    && state.edit_state.is_none()
                {
                    let step = state.preview_md.body_em().max(8.0);
                    match &event.logical_key {
                        Key::Character(s) if s.as_str() == "h" => {
                            state.md_table_scroll_px = (state.md_table_scroll_px - step).max(0.0);
                            state.window.request_redraw();
                            return;
                        }
                        Key::Character(s) if s.as_str() == "l" => {
                            // Clamp happens in redraw against the
                            // widest table's natural_w_px; harmless to
                            // overshoot here.
                            state.md_table_scroll_px += step;
                            state.window.request_redraw();
                            return;
                        }
                        Key::Character(s) if s.as_str() == "0" => {
                            state.md_table_scroll_px = 0.0;
                            state.window.request_redraw();
                            return;
                        }
                        // ArrowLeft / ArrowRight mirror h/l so the
                        // pan UX matches the PNG pan/zoom convention.
                        // Gated against `preview_png.is_none()` above
                        // so PNG previews retain their arrow-driven
                        // pan via the Action::PreviewPngPan* bindings.
                        Key::Named(NamedKey::ArrowLeft) => {
                            state.md_table_scroll_px = (state.md_table_scroll_px - step).max(0.0);
                            state.window.request_redraw();
                            return;
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            state.md_table_scroll_px += step;
                            state.window.request_redraw();
                            return;
                        }
                        _ => {}
                    }
                }
                // Focus-dispatched handling. NavTree = tree nav + mode
                // switches; Repl = code typing + Enter to submit. Preview
                // and Llm are passive today — Escape returns focus to the
                // tree so the user is never stranded with no input target.
                match state.focus {
                    PaneFocus::NavTree => {
                        // Sessions-mode create-session input (B4): when
                        // the prompt is active, key events route into the
                        // label buffer instead of the usual nav shortcuts.
                        // Enter confirms, Esc cancels, Backspace pops one
                        // char, plain Char appends. Other keys ignored
                        // (no arrows / no q-to-exit) so the user isn't
                        // surprised by mode-switch shortcuts inside what
                        // visually looks like text input.
                        // Workspace picker is active (ADR 0014). The
                        // NavTree key handler routes navigation into the
                        // picker's directory tree instead of the regular
                        // Sessions list. Up/Down moves cursor; Right
                        // drills into the cursored sub-dir; Left/Backspace
                        // ascends to parent; Enter commits the cursored
                        // directory as the new workspace (with the ccb
                        // agent), Shift+Enter commits it as a bare session
                        // (no LLM agent); Esc cancels. q is intentionally
                        // *not* a quit shortcut here so the user can still
                        // type single chars later.
                        if state.workspace_picker.is_some() {
                            // Up/Down repeats so hold-to-scroll feels
                            // right. Enter on the cursored sub-directory
                            // is the "this is the one" gesture and commits
                            // it as the workspace root (Shift+Enter for a
                            // bare, agent-less session). Right is the
                            // no-commit preview path (drill in without
                            // selecting). Left / Backspace walks back to
                            // the parent. Esc cancels.
                            // Commit is keymap-driven (.sot/keybindings.toml:
                            // session.create / session.create_bare) for no-recompile
                            // reconfig. Check the bare (Shift+Enter) chord BEFORE
                            // the plain-Enter chord: a non-shift "Enter" chord also
                            // matches when shift is held, so order disambiguates.
                            if !event.repeat
                                && state.bindings.matches(
                                    Action::SessionCreateCodex,
                                    &event.logical_key,
                                    ctrl,
                                    alt,
                                    shift,
                                )
                            {
                                state.picker_confirm_selected("codex");
                                return;
                            }
                            if !event.repeat
                                && state.bindings.matches(
                                    Action::SessionCreateBare,
                                    &event.logical_key,
                                    ctrl,
                                    alt,
                                    shift,
                                )
                            {
                                state.picker_confirm_selected("none");
                                return;
                            }
                            if !event.repeat
                                && state.bindings.matches(
                                    Action::SessionCreate,
                                    &event.logical_key,
                                    ctrl,
                                    alt,
                                    shift,
                                )
                            {
                                state.picker_confirm_selected("claude");
                                return;
                            }
                            match &event.logical_key {
                                Key::Named(NamedKey::ArrowDown) => {
                                    state.picker_cursor_down();
                                    return;
                                }
                                Key::Named(NamedKey::ArrowUp) => {
                                    state.picker_cursor_up();
                                    return;
                                }
                                Key::Named(NamedKey::ArrowRight) if !event.repeat => {
                                    state.picker_drill_in();
                                    return;
                                }
                                Key::Named(NamedKey::ArrowLeft)
                                | Key::Named(NamedKey::Backspace)
                                    if !event.repeat =>
                                {
                                    state.picker_ascend();
                                    return;
                                }
                                Key::Named(NamedKey::Escape) if !event.repeat => {
                                    state.picker_cancel();
                                    return;
                                }
                                _ => {
                                    return;
                                }
                            }
                        }
                        // NavTree text prompt active (Ctrl+N new-file, and
                        // future delete-confirm). Like the picker, it steals
                        // every keystroke so the user can type a filename
                        // without nav shortcuts firing: printable chars
                        // append (path separators are dropped at the source),
                        // Backspace pops, Enter confirms, Esc cancels, and
                        // any other nav key is swallowed so arrows / mode
                        // switches don't disturb the tree mid-type.
                        if state.nav_prompt.is_some() {
                            // ConfirmDelete is a y/N gate, not a text field:
                            // 'y'/'Y' confirms, everything else (incl.
                            // 'n'/'N'/Esc) cancels. CreateFile keeps its
                            // text-input behaviour below — branch on variant.
                            if matches!(state.nav_prompt, Some(NavPrompt::ConfirmDelete { .. })) {
                                match &event.logical_key {
                                    Key::Character(s)
                                        if !ctrl
                                            && !alt
                                            && !super_
                                            && s.eq_ignore_ascii_case("y") =>
                                    {
                                        state.confirm_delete_file();
                                        return;
                                    }
                                    _ => {
                                        // 'n'/'N'/Esc/any other key → cancel.
                                        state.cancel_nav_prompt();
                                        return;
                                    }
                                }
                            }
                            match &event.logical_key {
                                Key::Named(NamedKey::Enter) if !event.repeat => {
                                    state.confirm_create_file();
                                    return;
                                }
                                Key::Named(NamedKey::Escape) if !event.repeat => {
                                    state.cancel_nav_prompt();
                                    return;
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    state.nav_prompt_backspace();
                                    return;
                                }
                                Key::Character(s) => {
                                    // A character key with a modifier other
                                    // than Shift (Ctrl/Alt/Super) isn't text
                                    // — swallow it rather than typing the
                                    // letter. Plain + Shift chars append.
                                    if !ctrl && !alt && !super_ {
                                        for c in s.chars() {
                                            state.nav_prompt_push_char(c);
                                        }
                                    }
                                    return;
                                }
                                _ => {
                                    return;
                                }
                            }
                        }
                        // NavTree focus: Ctrl+Q is the *only* way to exit
                        // the interactive window. Plain q and Esc no longer
                        // quit — Esc is used constantly in the LLM pane
                        // (vim, readline interrupt, claude prompt cancel)
                        // and the user routinely double-taps it; making
                        // it lethal turned every reflex into a quit risk.
                        // Ctrl+Q is scoped to NavTree only so it doesn't
                        // collide with terminal flow-control (XOFF) in
                        // the BL pty. Capture mode sets `should_exit` on
                        // its own and never sees user input.
                        if !event.repeat
                            && state.bindings.matches(
                                Action::Quit,
                                &event.logical_key,
                                ctrl,
                                alt,
                                shift,
                            )
                        {
                            state.should_exit = true;
                            event_loop.exit();
                            return;
                        }
                        // Ctrl+C: copy the cursored row's file path to the
                        // OS clipboard. Only fires for `files:`-prefixed
                        // node ids (Files mode + Modules-mode rows that
                        // reuse the synthesized files: id for previews);
                        // sessions / picker / workspace rows pass through.
                        // Ctrl+C is reserved-as-interrupt in the LLM pty
                        // but in NavTree there's no pty, so the universal
                        // copy convention reads cleanly here.
                        if !event.repeat
                            && ctrl
                            && !shift
                            && !alt
                            && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("c"))
                            && state.copy_navtree_path()
                        {
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // Ctrl+N: open the new-file prompt. Files mode only,
                        // and only when the cursor sits on a `files:` row
                        // (begin_create_file no-ops otherwise and falls
                        // through to normal nav). Reuses `file.write` with
                        // empty content — no backend op is added.
                        if !event.repeat
                            && ctrl
                            && !shift
                            && !alt
                            && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("n"))
                            && matches!(state.mode, Mode::Files)
                            && state.begin_create_file()
                        {
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // Ctrl+D: open the delete-confirm prompt. Files mode
                        // only, and only when the cursor sits on a deletable
                        // `files:` file row (begin_delete_file no-ops / pre-
                        // refuses dirs otherwise and falls through to normal
                        // nav). This is the NavTree-focus Ctrl+D; the preview-
                        // focus Ctrl+D (half-page scroll) is a separate block.
                        if !event.repeat
                            && ctrl
                            && !shift
                            && !alt
                            && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("d"))
                            && matches!(state.mode, Mode::Files)
                            && state.begin_delete_file()
                        {
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        match &event.logical_key {
                            Key::Named(NamedKey::ArrowDown) => {
                                state.tree.move_down();
                            }
                            Key::Named(NamedKey::ArrowUp) => {
                                state.tree.move_up();
                            }
                            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Enter)
                                if !event.repeat =>
                            {
                                // Enter in Sessions mode dispatches to the
                                // right action based on row kind:
                                //   session_create → open the label prompt (B4)
                                //   session / pane → attach BL to that session (B3)
                                //   anything else  → fall through to expand
                                // Right keeps the pure-expand behaviour so
                                // users can explore the panes list without
                                // re-targeting the BL pane.
                                let is_enter =
                                    matches!(event.logical_key, Key::Named(NamedKey::Enter));
                                if is_enter && matches!(state.mode, Mode::Hosts) {
                                    // ADR 0015: persist the selected host
                                    // so the next launcher run targets
                                    // it. We don't tear down the live
                                    // transport — that would require a
                                    // sentinel-file protocol with the
                                    // launcher. v1 is "set + quit +
                                    // relaunch" (Ctrl+Q exits cleanly,
                                    // shortcut respawns the supervisor).
                                    state.pick_host_under_cursor();
                                    return;
                                }
                                if is_enter && matches!(state.mode, Mode::Sessions) {
                                    let kind = state
                                        .tree
                                        .rows
                                        .get(state.tree.selected)
                                        .map(|r| r.node.kind.clone());
                                    match kind.as_deref() {
                                        Some("session_create") => {
                                            state.begin_create_session();
                                            return;
                                        }
                                        Some("session") | Some("pane") => {
                                            if let Some(session_name) =
                                                state.selected_session_name()
                                            {
                                                // ADR 0014: route the swap
                                                // through the unified entry
                                                // point. The slug is the
                                                // session name with the
                                                // `sot-be-` prefix
                                                // stripped (the backend's
                                                // resolve() accepts either
                                                // a workspace_id or a slug).
                                                let slug = session_name
                                                    .strip_prefix("sot-be-")
                                                    .map(|s| s.to_string());
                                                if slug.is_some() {
                                                    state.switch_to_workspace(
                                                        slug,
                                                        Some(session_name),
                                                    );
                                                } else {
                                                    // Foreign tmux session
                                                    // surfaced by an older
                                                    // backend that hadn't
                                                    // filtered them out —
                                                    // just retarget BL.
                                                    state.attach_session_to_bl(session_name);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                state.try_expand_selected();
                            }
                            Key::Named(NamedKey::ArrowLeft) if !event.repeat => {
                                if !state.tree.collapse_selected() {
                                    if let Some(p) = state.tree.parent_of_selected() {
                                        state.tree.selected = p;
                                    }
                                }
                            }
                            // Mode switches are keymap-driven (mode.files /
                            // mode.modules / mode.sessions / mode.hosts) via
                            // match guards, so the default single-char chords
                            // (f/m/s/h) stay literal text everywhere else — this
                            // arm only runs inside the nav-focus match.
                            k if !event.repeat
                                && state.bindings.matches(
                                    Action::ModeFiles,
                                    k,
                                    ctrl,
                                    alt,
                                    shift,
                                ) =>
                            {
                                state.enter_mode(Mode::Files);
                            }
                            k if !event.repeat
                                && state.bindings.matches(
                                    Action::ModeModules,
                                    k,
                                    ctrl,
                                    alt,
                                    shift,
                                ) =>
                            {
                                state.enter_mode(Mode::Modules);
                            }
                            k if !event.repeat
                                && state.bindings.matches(
                                    Action::ModeSessions,
                                    k,
                                    ctrl,
                                    alt,
                                    shift,
                                ) =>
                            {
                                state.enter_mode(Mode::Sessions);
                            }
                            // C2 pin-and-leave: `p` toggles pin on the
                            // cursor row. Only meaningful in Files mode
                            // — `toggle_pin` filters rows whose id
                            // doesn't start with `files:`.
                            Key::Character(s) if !event.repeat && s.as_str() == "p" => {
                                state.toggle_pin();
                            }
                            // ADR 0015 — `h` enters Mode::Hosts, populating
                            // the nav tree from `hosts.toml`. No backend
                            // round-trip needed: hosts.toml is read at
                            // startup and lives entirely on the frontend
                            // side. Cursor on the currently-selected host
                            // is the natural way in.
                            k if !event.repeat
                                && state.bindings.matches(
                                    Action::ModeHosts,
                                    k,
                                    ctrl,
                                    alt,
                                    shift,
                                ) =>
                            {
                                state.enter_mode(Mode::Hosts);
                            }
                            // `.` toggles hidden dotfiles in Files mode
                            // (nav-focus-gated via the keymap so it stays
                            // literal text in the pty/editor/prompts). Sends
                            // nav.toggle_hidden + re-fetches the files tree.
                            k if !event.repeat
                                && state.bindings.matches(
                                    Action::ToggleHidden,
                                    k,
                                    ctrl,
                                    alt,
                                    shift,
                                ) =>
                            {
                                state.toggle_hidden_files();
                            }
                            // Capital D (Shift+d) in Sessions mode →
                            // destroy the cursor row's workspace. Two-
                            // press confirm via `was_destroy_pending`:
                            // first press arms with the target id, the
                            // status line tells the user; second press
                            // on the same row fires `workspace.destroy`.
                            // Cursor move, mode switch, or any other
                            // key clears the arm (handled by the
                            // snapshot-and-clear at the top of this
                            // handler). Default workspace is rejected
                            // backend-side; that surface as a status
                            // error.
                            Key::Character(s)
                                if !event.repeat
                                // caps-lock-immune: Shift+d under Caps Lock arrives as "d"
                                && (s.as_str() == "D" || (shift && s.eq_ignore_ascii_case("D")))
                                && matches!(state.mode, Mode::Sessions) =>
                            {
                                let Some(row) = state.tree.rows.get(state.tree.selected) else {
                                    return;
                                };
                                if row.node.kind != "session" {
                                    return;
                                }
                                let target_id = row
                                    .node
                                    .payload
                                    .get("workspace_id")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                                let target_label = row
                                    .node
                                    .payload
                                    .get("label")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(row.node.label.as_str())
                                    .to_string();
                                let Some(target_id) = target_id else {
                                    state.status =
                                        "destroy: row has no workspace_id (refresh `s` and retry)"
                                            .to_string();
                                    state.window.request_redraw();
                                    return;
                                };
                                if was_destroy_pending.as_deref() == Some(target_id.as_str()) {
                                    if let Err(e) = state.req_tx.send(
                                        crate::transport::OutgoingReq::WorkspaceDestroy {
                                            workspace_id: target_id.clone(),
                                        },
                                    ) {
                                        tracing::warn!(error = %e, "drop workspace.destroy");
                                        state.status = format!(
                                            "destroy '{target_label}' failed · channel closed"
                                        );
                                    } else {
                                        state.status = format!("destroying '{target_label}'…");
                                    }
                                    state.window.request_redraw();
                                } else {
                                    state.pending_destroy_target = Some(target_id);
                                    state.status =
                                        format!("press D again to destroy '{target_label}' · any other key cancels");
                                    state.window.request_redraw();
                                }
                            }
                            // `o` opens the cursored row in an external
                            // tool: text/html previews → temp file + OS
                            // browser; .jl files → backend `pluto.open`
                            // (header-checked on the backend, returns
                            // `not_pluto_flavored` for raw .jl). Routed
                            // by the cursored row's path, not preview
                            // mime — the JuliaSource plugin renders .jl
                            // as tokens-JSON.
                            Key::Character(s) if !event.repeat && s.as_str() == "o" => {
                                let cursored = state.cursored_files_path();
                                state.open_path_external(cursored);
                            }
                            // `W` (Shift+W): open the project's built Documenter
                            // site in the OS browser with full CSS/JS/sub-page
                            // fidelity (ADR 0024). Backend serves `docs/build`
                            // over a forwarded loopback port. Sends the cursored
                            // path so a built docs page deep-links; otherwise the
                            // backend opens the index. `W` works from any mode.
                            Key::Character(s)
                                if !event.repeat
                                    // caps-lock-immune (maintainer report, 2026-07-03: "Shift+W isn't
                                    // working" — Caps Lock made it arrive as "w")
                                    && (s.as_str() == "W" || (shift && s.eq_ignore_ascii_case("W"))) =>
                            {
                                let path = state.cursored_files_path().unwrap_or_default();
                                state.docs_open_external(path);
                            }
                            // `O` (Shift+O): full render WITH code execution
                            // for a cursored `.qmd`, then open in the browser.
                            // Slower + needs the language kernels on the backend
                            // host; `o` is the fast no-execute path.
                            Key::Character(s)
                                if !event.repeat
                                    && (s.as_str() == "O"
                                        || (shift && s.eq_ignore_ascii_case("O"))) =>
                            {
                                let cursored = state.cursored_files_path();
                                state.quarto_open_execute(cursored);
                            }
                            // `d`: download the cursored file row to the local
                            // OS downloads dir (OS-independent), non-clobbering.
                            // Transport streams chunks; dir rows are a no-op.
                            Key::Character(s)
                                if !event.repeat && !ctrl && !alt && s.as_str() == "d" =>
                            {
                                state.start_download();
                            }
                            // `u`: pick a local file via the native OS dialog
                            // and upload it to the cursored nav folder (the dir
                            // itself for a dir row, else the file's parent).
                            Key::Character(s)
                                if !event.repeat && !ctrl && !alt && s.as_str() == "u" =>
                            {
                                state.start_upload();
                            }
                            // Priority J: `r` resets the workspace's
                            // persistent REPL into the file's closest-
                            // ancestor Project.toml then include()s the
                            // file. `R` (Shift+r) just include()s in the
                            // existing REPL — no env change. Both gate
                            // on a `.jl` cursored row; non-.jl rows are
                            // a no-op. Output flows back through the
                            // existing repl frame stream into the REPL
                            // drawer. Future: mirror the last image
                            // frame to the preview pane (TODO row 161).
                            Key::Character(s)
                                if !event.repeat && (s.as_str() == "r" || s.as_str() == "R") =>
                            {
                                let Some(abs) = state.cursored_files_path() else {
                                    return;
                                };
                                if !abs.ends_with(".jl") {
                                    tracing::debug!(path = %abs,
                                        "`r`/`R` ignored — not a .jl file");
                                    return;
                                }
                                let fresh = s.as_str() == "r";
                                let basename = abs
                                    .rsplit(['/', '\\'])
                                    .next()
                                    .unwrap_or(abs.as_str())
                                    .to_string();
                                // `r` resets the REPL *process* on the backend
                                // (fresh `julia --project=…`), so reset the
                                // drawer window to match — the old scrollback
                                // belongs to a now-dead session. `R` keeps the
                                // existing session and its scrollback. History
                                // derives from `repl_log`, so clearing the log
                                // clears it too; the eval counter keeps
                                // monotonically rising to avoid eval_id reuse
                                // with any still-draining replies.
                                if fresh {
                                    state.repl_log.clear();
                                    state.repl_scroll = 0;
                                    state.repl_pkg_mode = false;
                                    state.history_pos = None;
                                    state.history_saved = None;
                                }
                                // Pre-register a `repl_log` entry exactly the way
                                // `submit_repl_input` does for repl.eval, so the
                                // ReplRunFileDone reply can splice frames in by
                                // eval_id and the drawer scrollback shows the
                                // run's output alongside everything else.
                                state.repl_eval_counter = state.repl_eval_counter.saturating_add(1);
                                let eval_id = state.repl_eval_counter;
                                let workspace_key = state.current_workspace_key();
                                state
                                    .eval_id_workspace
                                    .insert(eval_id, workspace_key.clone());
                                if state.repl_log.len() >= 256 {
                                    let excess = state.repl_log.len() - 255;
                                    state.repl_log.drain(0..excess);
                                }
                                let synthetic_code = format!("{} {}", s.as_str(), abs);
                                state.repl_log.push(ReplEntry {
                                    eval_id,
                                    code: synthetic_code,
                                    frames: Vec::new(),
                                    elapsed_ms: 0,
                                    in_flight: true,
                                    pkg_mode: false,
                                });
                                if let Err(e) =
                                    state
                                        .req_tx
                                        .send(crate::transport::OutgoingReq::ReplRunFile {
                                            eval_id,
                                            path: abs.clone(),
                                            fresh,
                                            workspace_id: state.active_workspace_id.clone(),
                                        })
                                {
                                    tracing::warn!(error = %e,
                                        "failed to dispatch repl.run_file");
                                    if let Some(entry) =
                                        state.repl_log.iter_mut().find(|e| e.eval_id == eval_id)
                                    {
                                        entry.in_flight = false;
                                        entry.frames.push(sot_protocol::ReplFrame::Error {
                                            message: format!("transport channel closed: {e}"),
                                            stacktrace: Vec::new(),
                                        });
                                    }
                                    state.status = format!(
                                        "repl.run_file '{basename}' failed · channel closed"
                                    );
                                } else if fresh {
                                    state.status =
                                        format!("running '{basename}' (resetting REPL …)");
                                } else {
                                    state.status = format!("running '{basename}' (existing repl)");
                                }
                                // Auto-open (or switch to) the REPL drawer so
                                // the run's output is visible (settings-gated,
                                // default on). If the Terminal drawer is up we
                                // swap it for the REPL since that's where the
                                // output lands. Keep NavTree focus so `r`/`R`
                                // stay usable — unlike Ctrl+J this does not
                                // steal focus.
                                if state.settings.repl_auto_open_drawer_on_run
                                    && state.drawer != DrawerContent::Repl
                                    && state
                                        .settings
                                        .resolve_preset(state.monitor_aspect)
                                        .drawer
                                        .is_some()
                                {
                                    state.drawer = DrawerContent::Repl;
                                }
                                state.window.request_redraw();
                            }
                            _ => {}
                        }
                    }
                    PaneFocus::Repl => {
                        // Ctrl+L — clear the REPL drawer scrollback (the
                        // universal REPL-clear; maintainer note, 2026-07-03, "how do I
                        // clear the repl"). Clears the log, its decoded
                        // inline figures, and the scroll offset; the julia
                        // process and its state are untouched (`r` on a .jl
                        // is the process-restart gesture). Terminal drawer
                        // unaffected — its pty owns Ctrl+L natively.
                        if !event.repeat
                            && ctrl
                            && !alt
                            && state.drawer != DrawerContent::Terminal
                            && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("l"))
                        {
                            state.repl_log.clear();
                            state.repl_images.clear();
                            state.repl_image_slots.clear();
                            state.repl_scroll = 0;
                            state.status = "repl · scrollback cleared".to_string();
                            state.window.request_redraw();
                            return;
                        }
                        // Paste shortcut (Ctrl+V / Cmd+V / Shift+Insert):
                        // read the OS clipboard. The Terminal drawer gets it
                        // as a bracketed-paste blob on its pty (like the LLM
                        // pane); the Julia REPL drawer gets it appended to
                        // its input buffer. Intercepted before the
                        // terminal/REPL split below, where Ctrl+V would
                        // otherwise send a bare 0x16 to the pty or type a
                        // literal "v" into the buffer.
                        let is_paste_shortcut = !event.repeat
                            && match &event.logical_key {
                                Key::Character(s)
                                    if s.eq_ignore_ascii_case("v") && (ctrl || super_) =>
                                {
                                    true
                                }
                                Key::Named(NamedKey::Insert) if shift => true,
                                _ => false,
                            };
                        if is_paste_shortcut {
                            if state.drawer == DrawerContent::Terminal {
                                forward_clipboard_paste_to_local_term(state);
                            } else if let Some(text) = read_clipboard_text() {
                                // REPL input is an editable buffer, not a pty
                                // — no bracketed-paste envelope. Normalize to
                                // `\n`; embedded newlines stay in the buffer
                                // (Shift+Enter inserts them too) and the user
                                // submits with Enter.
                                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                                state.repl_input.push_str(&normalized);
                                state.repl_scroll = 0;
                            }
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // G4: when the drawer is showing the local terminal,
                        // every keystroke is forwarded to its PTY and the
                        // REPL input/history/scrollback handling below is
                        // bypassed entirely. Drawer toggles (Ctrl+T / Ctrl+J)
                        // and Ctrl+Arrow are intercepted globally before this
                        // arm, so they still exit/switch the pane; Tab falls
                        // through to the PTY for shell completion.
                        if state.drawer == DrawerContent::Terminal {
                            // Shift+PageUp/PageDown drive our scrollback ring
                            // (the conventional emulator-scrollback chord that
                            // apps ignore), so claude/the shell keep plain
                            // PageUp. One-third-pane step like the REPL pane.
                            let h = state.pane_rects.repl.height as i32;
                            let page_step = (h / 3).max(1);
                            if shift {
                                match &event.logical_key {
                                    Key::Named(NamedKey::PageUp) => {
                                        let new = (state.term_scroll as i32 + page_step).max(0);
                                        state.term_scroll = new as u16;
                                        state.window.request_redraw();
                                        return;
                                    }
                                    Key::Named(NamedKey::PageDown) => {
                                        let new = (state.term_scroll as i32 - page_step).max(0);
                                        state.term_scroll = new as u16;
                                        state.window.request_redraw();
                                        return;
                                    }
                                    _ => {}
                                }
                            }
                            if let Some(bytes) = key_to_pty_bytes(&event.logical_key, ctrl) {
                                if let Some(t) = state.local_term.as_mut() {
                                    t.send_input(&bytes);
                                }
                                // Typing snaps back to the live tail so the
                                // cursor/prompt is visible (standard emulator
                                // behaviour).
                                state.term_scroll = 0;
                                state.last_key = Some(label);
                                state.window.request_redraw();
                            }
                            return;
                        }
                        // Scrollback navigation intercepts before any
                        // input-buffer arms, so PgUp/PgDn / Ctrl+u/d
                        // don't try to also type into repl_input. Held
                        // keys repeat (no `!event.repeat` guard) so
                        // hold-to-scroll feels natural. Sign flip vs
                        // preview: REPL's scroll origin is the tail,
                        // so PgUp grows the offset (older). PgUp/PgDn
                        // step is one-third of the pane (not a full
                        // page) so two rows of context survive the
                        // scroll; full-page jumps were dropping the
                        // user out of where they were reading.
                        let h = state.pane_rects.repl.height as i32;
                        let page_step = (h / 3).max(1);
                        match &event.logical_key {
                            Key::Named(NamedKey::PageUp) => {
                                let new = (state.repl_scroll as i32 + page_step).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            Key::Named(NamedKey::PageDown) => {
                                let new = (state.repl_scroll as i32 - page_step).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            Key::Character(s) if ctrl && s.as_str() == "u" => {
                                let new = (state.repl_scroll as i32 + h / 2).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            Key::Character(s) if ctrl && s.as_str() == "d" => {
                                let new = (state.repl_scroll as i32 - h / 2).max(0);
                                state.repl_scroll = new as u16;
                                state.window.request_redraw();
                                return;
                            }
                            // Ctrl+C interrupts a running eval (repl.interrupt).
                            // Only dispatched when something is actually in
                            // flight: the backend schedules an InterruptException
                            // into the eval task and the error+done frames stream
                            // back to finalize the entry (no eval_id -- the kernel
                            // interrupts its CURRENT_EVAL). With nothing running,
                            // Ctrl+C clears the input line (standard REPL UX)
                            // instead of typing a literal 'c'.
                            Key::Character(s) if ctrl && s.as_str() == "c" => {
                                if state.repl_log.iter().any(|e| e.in_flight) {
                                    if let Err(e) = state.req_tx.send(
                                        crate::transport::OutgoingReq::ReplInterrupt {
                                            workspace_id: state.active_workspace_id.clone(),
                                        },
                                    ) {
                                        tracing::warn!(error = %e, "drop repl.interrupt - channel closed");
                                    } else {
                                        tracing::info!("repl.interrupt dispatched (Ctrl+C)");
                                        state.status = "interrupting...".to_string();
                                    }
                                } else {
                                    state.repl_input.clear();
                                    state.repl_pkg_mode = false;
                                }
                                state.repl_scroll = 0;
                                state.window.request_redraw();
                                return;
                            }
                            // Up/Down walk REPL history. Allowed to repeat
                            // so hold-to-walk feels natural. Returns
                            // early so the input-buffer match below
                            // doesn't also see the keypress.
                            Key::Named(NamedKey::ArrowUp) => {
                                if let Some(prev) = state.history_step_back() {
                                    state.repl_input = prev;
                                    state.repl_scroll = 0;
                                    state.window.request_redraw();
                                }
                                return;
                            }
                            Key::Named(NamedKey::ArrowDown) => {
                                if let Some(next) = state.history_step_forward() {
                                    state.repl_input = next;
                                    state.repl_scroll = 0;
                                    state.window.request_redraw();
                                }
                                return;
                            }
                            _ => {}
                        }
                        match &event.logical_key {
                            // Escape returns focus to the tree pane; it
                            // does NOT exit the app from inside the REPL
                            // — exit only happens from tree focus, which
                            // is the safer default for an input pane.
                            Key::Named(NamedKey::Escape) if !event.repeat => {
                                state.focus = PaneFocus::NavTree;
                            }
                            // Shift+Enter inserts a literal newline into
                            // the input buffer instead of submitting —
                            // mirrors the convention used by Slack /
                            // Discord / VS Code REPLs and a handful of
                            // shells. Repeat is allowed so hold-down
                            // appends multiple blank lines.
                            Key::Named(NamedKey::Enter) if shift => {
                                state.repl_input.push('\n');
                                state.repl_scroll = 0;
                            }
                            Key::Named(NamedKey::Enter) if !event.repeat => {
                                state.submit_repl_input();
                                // Snap back to live when the user
                                // commits a line — the new entry is at
                                // the tail and the user expects to see
                                // its output.
                                state.repl_scroll = 0;
                            }
                            Key::Named(NamedKey::Backspace) => {
                                // Backspace at start of empty input in
                                // pkg mode leaves pkg mode — mirrors
                                // the standard Julia REPL UX.
                                if state.repl_input.is_empty() && state.repl_pkg_mode {
                                    state.repl_pkg_mode = false;
                                } else {
                                    state.repl_input.pop();
                                }
                                state.repl_scroll = 0;
                            }
                            Key::Named(NamedKey::Space) => {
                                state.repl_input.push(' ');
                                state.repl_scroll = 0;
                            }
                            Key::Character(s) => {
                                // `]` at start of empty input enters
                                // pkg mode and consumes the keypress —
                                // again mirroring the standard REPL.
                                if s.as_str() == "]"
                                    && state.repl_input.is_empty()
                                    && !state.repl_pkg_mode
                                {
                                    state.repl_pkg_mode = true;
                                    state.repl_scroll = 0;
                                } else {
                                    // Append the typed string verbatim.
                                    // winit honours shift/IME so
                                    // casing/accents already arrive
                                    // correctly. Filter only control
                                    // chars so stray sequences don't
                                    // leak into the buffer.
                                    for c in s.chars() {
                                        if !c.is_control() {
                                            state.repl_input.push(c);
                                        }
                                    }
                                    state.repl_scroll = 0;
                                }
                            }
                            _ => {}
                        }
                    }
                    PaneFocus::Preview => {
                        // Edit mode hijacks all keys — typing into the
                        // editable annotation body takes precedence
                        // over scroll keys. Ctrl+S saves; Esc discards
                        // (commit 3 adds the dirty-confirm modal).
                        if let Some(edit) = state.edit_state.as_mut() {
                            // 1/3-pane step here too so the editor's
                            // cursor doesn't blow past visible context
                            // on PgUp/PgDn.
                            let page_rows = (state.pane_rects.preview.height as usize / 3).max(1);
                            // When the discard-confirm modal is up,
                            // the key handler is just y/n/Esc. Any
                            // other key dismisses the modal and
                            // returns to editing without consuming
                            // the character (felt safer than letting
                            // a stray keystroke leak into the buffer
                            // during a confirmation).
                            if edit.confirm_discard {
                                match &event.logical_key {
                                    // `y` or a second Esc → discard edits and
                                    // leave the editor (Esc-to-confirm-exit,
                                    // chosen 2026-06-09: first Esc raises this
                                    // prompt, a second Esc exits). `n` or any
                                    // other key cancels back to editing.
                                    Key::Named(NamedKey::Escape) if !event.repeat => {
                                        state.edit_state = None;
                                        state.preview_edit = None;
                                    }
                                    Key::Character(s)
                                        if !event.repeat
                                            && s.as_str().eq_ignore_ascii_case("y") =>
                                    {
                                        state.edit_state = None;
                                        state.preview_edit = None;
                                    }
                                    _ => {
                                        edit.confirm_discard = false;
                                    }
                                }
                                state.window.request_redraw();
                                return;
                            }
                            // Stale banner intercepts before edit keys
                            // too — r reloads from disk (discards
                            // edits), k keeps the banner dismissed so
                            // the user can keep editing. The next save
                            // will fail again until the underlying
                            // file changes or the user reloads.
                            if edit.stale_banner {
                                match &event.logical_key {
                                    Key::Character(s)
                                        if !event.repeat
                                            && s.as_str().eq_ignore_ascii_case("r") =>
                                    {
                                        // Reload from disk, discarding edits. For
                                        // a file edit re-fire file.read (the
                                        // FileRead handler replaces the buffer +
                                        // clears the banner via a fresh
                                        // edit_state); for a concept edit re-fire
                                        // concept.read.
                                        let ws = state.active_workspace_id.clone();
                                        if let Some(node_id) = edit.file_node_id.clone() {
                                            state.pending_file_edit = Some(node_id.clone());
                                            if let Err(e) =
                                                state.req_tx.send(OutgoingReq::FileRead {
                                                    node_id,
                                                    workspace_id: ws,
                                                })
                                            {
                                                tracing::warn!(error = %e,
                                                    "drop file.read for stale reload");
                                            }
                                        } else {
                                            let target = edit.target.clone();
                                            if let Err(e) =
                                                state.req_tx.send(OutgoingReq::ConceptRead {
                                                    target,
                                                    workspace_id: ws,
                                                })
                                            {
                                                tracing::warn!(error = %e,
                                                    "drop concept.read for stale reload");
                                            }
                                        }
                                    }
                                    Key::Character(s)
                                        if !event.repeat
                                            && s.as_str().eq_ignore_ascii_case("k") =>
                                    {
                                        edit.stale_banner = false;
                                    }
                                    _ => {
                                        // Any other key: ignored.
                                        // Banner stays up until the
                                        // user picks r or k.
                                    }
                                }
                                state.rebuild_edit_preview();
                                state.window.request_redraw();
                                return;
                            }
                            // Track whether the buffer changed so we
                            // only rebuild `preview_edit` when needed.
                            // Cheap either way (few-KB shape), but it
                            // keeps the trace log clean of redundant
                            // rebuilds during cursor-only navigation.
                            let mut buf_changed = false;
                            match &event.logical_key {
                                Key::Named(NamedKey::Escape) if !event.repeat => {
                                    // Dirty buffer → confirm modal.
                                    // Clean buffer → discard right
                                    // away (no value in asking when
                                    // there are no edits to lose).
                                    if edit.is_dirty() {
                                        edit.confirm_discard = true;
                                    } else {
                                        state.edit_state = None;
                                        state.preview_edit = None;
                                        // Re-fetch the underlying preview so
                                        // it reflects content saved during
                                        // this edit session — the cached
                                        // preview is the pre-edit render.
                                        // Clearing the fired-guard lets
                                        // maybe_fire_preview re-issue
                                        // preview.get for the still-selected
                                        // node.
                                        state.preview_node_id_fired = None;
                                        state.maybe_fire_preview();
                                    }
                                    state.window.request_redraw();
                                    return;
                                }
                                Key::Character(s) if ctrl && s.as_str() == "s" => {
                                    let content = edit.full_content();
                                    let ws = state.active_workspace_id.clone();
                                    if let Some(node_id) = edit.file_node_id.clone() {
                                        // General-file save → file.write, gated
                                        // on the version we read (conflict-aware).
                                        let expected_version = edit.file_version.clone();
                                        if let Err(e) = state.req_tx.send(OutgoingReq::FileWrite {
                                            node_id,
                                            content,
                                            expected_version,
                                            workspace_id: ws,
                                        }) {
                                            tracing::warn!(error = %e,
                                                "drop file.write — channel closed");
                                        }
                                    } else {
                                        // Concept-annotation save (existing path).
                                        let target = edit.target.clone();
                                        let expected = edit.expected_ast_hash.clone();
                                        if let Err(e) =
                                            state.req_tx.send(OutgoingReq::ConceptWrite {
                                                target,
                                                content,
                                                expected_ast_hash: expected,
                                                workspace_id: ws,
                                            })
                                        {
                                            tracing::warn!(error = %e,
                                                "drop concept.write — channel closed");
                                        }
                                    }
                                }
                                Key::Character(s) if ctrl && s.as_str() == "z" => {
                                    if edit.buf.undo() {
                                        buf_changed = true;
                                    }
                                }
                                Key::Character(s) if ctrl && s.as_str() == "y" => {
                                    if edit.buf.redo() {
                                        buf_changed = true;
                                    }
                                }
                                // Ctrl+C: copy the active selection to the OS
                                // clipboard. Consumed even with no selection so
                                // it never types a literal "c" into the buffer.
                                Key::Character(s)
                                    if ctrl && s.as_str().eq_ignore_ascii_case("c") =>
                                {
                                    if let Some(sel) = edit.buf.selected_text() {
                                        let text = sel.to_string();
                                        match arboard::Clipboard::new()
                                            .and_then(|mut cb| cb.set_text(text.clone()))
                                        {
                                            Ok(()) => tracing::info!(
                                                bytes = text.len(),
                                                "editor.copy → clipboard"
                                            ),
                                            Err(e) => tracing::warn!(
                                                error = %e,
                                                "clipboard write failed; editor copy dropped"
                                            ),
                                        }
                                    }
                                }
                                // Ctrl+X: cut — copy the selection, then delete
                                // it as one undo step. No selection → consumed
                                // no-op (never types an "x").
                                Key::Character(s)
                                    if ctrl && s.as_str().eq_ignore_ascii_case("x") =>
                                {
                                    if let Some(sel) = edit.buf.selected_text() {
                                        let text = sel.to_string();
                                        if let Err(e) = arboard::Clipboard::new()
                                            .and_then(|mut cb| cb.set_text(text))
                                        {
                                            tracing::warn!(
                                                error = %e,
                                                "clipboard write failed; editor cut still deletes"
                                            );
                                        }
                                        edit.buf.delete_selection();
                                        buf_changed = true;
                                    }
                                }
                                // Paste (Ctrl+V / Cmd+V): insert the OS
                                // clipboard as one atomic undo step. Without
                                // this arm Ctrl+V falls through to the generic
                                // Character arm below and types a literal "v".
                                // Normalize line endings to the buffer's `\n`
                                // convention (Enter inserts `\n`).
                                Key::Character(s)
                                    if (ctrl || super_) && s.as_str().eq_ignore_ascii_case("v") =>
                                {
                                    if let Some(text) = read_clipboard_text() {
                                        edit.buf.insert_str(
                                            &text.replace("\r\n", "\n").replace('\r', "\n"),
                                        );
                                        buf_changed = true;
                                    }
                                }
                                // Shift+Insert paste — same as Ctrl+V.
                                Key::Named(NamedKey::Insert) if shift => {
                                    if let Some(text) = read_clipboard_text() {
                                        edit.buf.insert_str(
                                            &text.replace("\r\n", "\n").replace('\r', "\n"),
                                        );
                                        buf_changed = true;
                                    }
                                }
                                Key::Named(NamedKey::Enter) => {
                                    edit.buf.insert_char('\n');
                                    buf_changed = true;
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    edit.buf.backspace();
                                    buf_changed = true;
                                }
                                Key::Named(NamedKey::Delete) => {
                                    edit.buf.delete();
                                    buf_changed = true;
                                }
                                // Motion keys: `set_selecting(shift)` extends a
                                // selection on Shift+motion and drops it on a
                                // plain motion. (Shift+Arrow no longer cycles
                                // workspaces here — that's gated to non-edit
                                // mode at the top of the key handler.)
                                Key::Named(NamedKey::ArrowLeft) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_left();
                                }
                                Key::Named(NamedKey::ArrowRight) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_right();
                                }
                                Key::Named(NamedKey::ArrowUp) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_up();
                                }
                                Key::Named(NamedKey::ArrowDown) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_down();
                                }
                                Key::Named(NamedKey::Home) if ctrl => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_buf_start();
                                }
                                Key::Named(NamedKey::End) if ctrl => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_buf_end();
                                }
                                Key::Named(NamedKey::Home) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_line_start();
                                }
                                Key::Named(NamedKey::End) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_line_end();
                                }
                                Key::Named(NamedKey::PageUp) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_up_rows(page_rows);
                                }
                                Key::Named(NamedKey::PageDown) => {
                                    edit.buf.set_selecting(shift);
                                    edit.buf.move_down_rows(page_rows);
                                }
                                Key::Named(NamedKey::Space) => {
                                    edit.buf.insert_char(' ');
                                    buf_changed = true;
                                }
                                Key::Character(s) => {
                                    for c in s.chars() {
                                        if !c.is_control() {
                                            edit.buf.insert_char(c);
                                            buf_changed = true;
                                        }
                                    }
                                }
                                _ => {}
                            }
                            // Cursor moves count as a content change for
                            // the preview because the injected `█` is
                            // part of the rendered string — rebuild
                            // unconditionally for now (cheap; can
                            // optimise later if profiling shows it).
                            let _ = buf_changed;
                            state.rebuild_edit_preview();
                            state.window.request_redraw();
                            return;
                        }
                        // Page transport for paginated previews (ADR 0021):
                        // n/p and PgDn/PgUp re-fire preview.get for the
                        // *shown* node at page ± 1 (clamped). Driven purely
                        // by the reply's page extras — the chrome never
                        // knows it's a PDF. Consumed even at the clamp edges
                        // so a stray press on page 1/N doesn't leak into
                        // other handlers; on NON-paginated previews PgUp/
                        // PgDn fall through to the text-scroll arms below.
                        // No autorepeat: each page is a fresh pdftoppm run.
                        if let Some((page, count)) = state.preview_page {
                            if count > 1 && !ctrl && !alt && !event.repeat {
                                {
                                    let next = match &event.logical_key {
                                        Key::Character(s) => match s.as_str() {
                                            "n" => Some(page.saturating_add(1).min(count)),
                                            "p" => Some(page.saturating_sub(1).max(1)),
                                            _ => None,
                                        },
                                        Key::Named(NamedKey::PageDown) => {
                                            Some(page.saturating_add(1).min(count))
                                        }
                                        Key::Named(NamedKey::PageUp) => {
                                            Some(page.saturating_sub(1).max(1))
                                        }
                                        _ => None,
                                    };
                                    if let Some(np) = next {
                                        if np != page {
                                            if let Some(node_id) =
                                                state.preview_node_id_fired.clone()
                                            {
                                                // New page opens at fit; drop
                                                // any pending zoom re-raster.
                                                state.preview_page_raster_pending = None;
                                                let (fit_w, fit_h) = state.preview_fit_px();
                                                if let Err(e) = state.req_tx.send(
                                                    crate::transport::OutgoingReq::PreviewGet {
                                                        node_id,
                                                        workspace_id: state
                                                            .active_workspace_id
                                                            .clone(),
                                                        page: Some(np),
                                                        fit_w,
                                                        fit_h,
                                                    },
                                                ) {
                                                    tracing::warn!(error = %e,
                                                        "drop page-turn preview.get — channel closed");
                                                }
                                            }
                                        }
                                        state.last_key = Some(label);
                                        state.window.request_redraw();
                                        return;
                                    }
                                }
                            }
                        }
                        // Esc → tree; PgUp/PgDn / Ctrl+u / Ctrl+d /
                        // Home / End scroll the preview's flowed text.
                        // Held keys repeat for hold-to-scroll. Viewport
                        // size is taken from the chrome cell height of
                        // the pane — close enough to a body line for
                        // the user not to notice the small mismatch
                        // with the mouse-wheel row math, and it keeps
                        // all four panes on the same rule.
                        let h = state.pane_rects.preview.height as i32;
                        // PNG-pane zoom/pan routed through `KeyBindings`
                        // so `.sot/keybindings.toml` can rebind each
                        // action. Defaults: zoom in/out is Shift+Arrow
                        // up/down (plus `+`/`=`/`-`); reset is `r` or
                        // `0`; pan is the bare arrows. Order matters —
                        // ZoomIn checked before PanUp so Shift+ArrowUp
                        // doesn't double-fire (the Chord matcher ignores
                        // surplus shift for the `=`/`+` compatibility
                        // case, so the same key can match both action
                        // lists; first-hit wins). Pan step is 10% of
                        // the pane size per press so the perceived
                        // increment is constant regardless of zoom; the
                        // render-time clamp keeps the canvas covering
                        // the pane.
                        if let Some(img_px) = state.preview_png.as_ref().map(|q| q.size_px) {
                            const ZOOM_STEP: f32 = 1.25;
                            const PAN_FRAC: f32 = 0.1;
                            let pane_w = state.pane_rects.preview.width as f32 * state.cell_w;
                            let pane_h = state.pane_rects.preview.height as f32 * state.cell_h;
                            // Zoom ceiling is per-image: how big a single
                            // source pixel may get on screen (16×16 px), not
                            // a fixed multiple of fit-to-pane. A dense raster
                            // whose native pixels are sub-screen-pixel at fit
                            // gets generous headroom; a tiny already-magnified
                            // image is held near fit.
                            let zoom_max = png_zoom_max(pane_w, pane_h, img_px);
                            let k = &event.logical_key;
                            let b = &state.bindings;
                            let mut handled = true;
                            if !event.repeat
                                && b.matches(Action::PreviewPngReset, k, ctrl, alt, shift)
                            {
                                state.preview_png_zoom = 1.0;
                                state.preview_png_pan_px = (0.0, 0.0);
                            } else if b.matches(Action::PreviewPngZoomIn, k, ctrl, alt, shift) {
                                // Zoom sequence: 1.0 → 1.25 → 2 → 3 → 4 → …
                                // up to the per-image ceiling (`zoom_max`).
                                // Once we're past 1.5×, step in integer
                                // multiples of fit so increments stay
                                // predictable and avoid the moiré beating of
                                // fractional zoom against the nearest-
                                // neighbour sampler grid — which keeps dense
                                // scientific rasters reading crisply per-pixel
                                // (user ask 2026-05-22). The final value is
                                // clamped to the ceiling, so the last step may
                                // land on a fractional zoom that puts a source
                                // pixel at exactly 16 screen px.
                                let cur = state.preview_png_zoom;
                                let raw_next = if cur < 1.5 {
                                    let raw = cur * ZOOM_STEP;
                                    if raw >= 1.5 {
                                        2.0
                                    } else {
                                        raw
                                    }
                                } else {
                                    cur + 1.0
                                };
                                let next = raw_next.clamp(1.0, zoom_max);
                                state.scale_png_pan_for_zoom(cur, next);
                                state.preview_png_zoom = next;
                            } else if b.matches(Action::PreviewPngZoomOut, k, ctrl, alt, shift) {
                                // Mirror of zoom-in: integer-step down
                                // from ≥ 2, then drop back through 1.25
                                // → 1.0. Hitting 2.0 → 1.25 is the
                                // discrete jump out of integer mode so
                                // the user lands cleanly on the
                                // multiplicative step below 1.5×.
                                let cur = state.preview_png_zoom;
                                let next = if cur > 1.5 {
                                    let raw = cur - 1.0;
                                    if raw < 2.0 {
                                        1.25
                                    } else {
                                        raw
                                    }
                                } else {
                                    (cur / ZOOM_STEP).max(1.0)
                                };
                                state.scale_png_pan_for_zoom(cur, next);
                                state.preview_png_zoom = next;
                                if state.preview_png_zoom <= 1.0 {
                                    state.preview_png_pan_px = (0.0, 0.0);
                                }
                            } else if b.matches(Action::PreviewPngPanLeft, k, ctrl, alt, shift) {
                                state.preview_png_pan_px.0 += pane_w * PAN_FRAC;
                            } else if b.matches(Action::PreviewPngPanRight, k, ctrl, alt, shift) {
                                state.preview_png_pan_px.0 -= pane_w * PAN_FRAC;
                            } else if b.matches(Action::PreviewPngPanUp, k, ctrl, alt, shift) {
                                state.preview_png_pan_px.1 += pane_h * PAN_FRAC;
                            } else if b.matches(Action::PreviewPngPanDown, k, ctrl, alt, shift) {
                                state.preview_png_pan_px.1 -= pane_h * PAN_FRAC;
                            } else {
                                handled = false;
                            }
                            if handled {
                                state.save_png_view();
                                // Paginated page (PDF): re-rasterize at the
                                // new zoom so text stays crisp past 1×.
                                state.maybe_reraster_page();
                                state.last_key = Some(label);
                                state.window.request_redraw();
                                return;
                            }
                        }
                        match &event.logical_key {
                            Key::Named(NamedKey::Escape) if !event.repeat => {
                                state.focus = PaneFocus::NavTree;
                            }
                            // ADR 0022: `c` captures the visible image ROI and
                            // sends it to the LLM pane. `capture_roi` no-ops
                            // with a status hint when the preview isn't a
                            // croppable image.
                            Key::Character(s) if !event.repeat && s.as_str() == "c" => {
                                state.capture_roi();
                            }
                            // `e` enters edit mode for the cursored
                            // annotation, if there is one. Per the
                            // 2026-05-15T21:32Z spec: modal text input,
                            // minimal scope, no auto-clobber on save.
                            // `y` (vim "yank") copies fenced code blocks in
                            // the current markdown preview to the system
                            // clipboard. Multiple blocks are joined with a
                            // blank line so a "copy everything" call still
                            // pastes cleanly into another editor. No-op
                            // when the preview isn't markdown or carries no
                            // code blocks.
                            Key::Character(s) if !event.repeat && s.as_str() == "y" => {
                                let sources = &state.preview_md.code_block_sources;
                                if !sources.is_empty() {
                                    let joined = sources.join("\n");
                                    let n = sources.len();
                                    match arboard::Clipboard::new()
                                        .and_then(|mut cb| cb.set_text(joined))
                                    {
                                        Ok(()) => tracing::info!(
                                            blocks = n,
                                            "yanked code block(s) to clipboard"
                                        ),
                                        Err(e) => tracing::warn!(
                                            error = %e,
                                            "failed to write code blocks to clipboard"
                                        ),
                                    }
                                }
                            }
                            // Open-style keys work from the preview pane too
                            // (same handlers as NavTree), acting on the file
                            // whose preview is SHOWING — pinned/badge-consumed
                            // previews can differ from the nav cursor — with
                            // fallback to the cursored row.
                            Key::Character(s) if !event.repeat && s.as_str() == "o" => {
                                let shown = state
                                    .previewed_files_path()
                                    .or_else(|| state.cursored_files_path());
                                state.open_path_external(shown);
                            }
                            Key::Character(s)
                                if !event.repeat
                                    && (s.as_str() == "W"
                                        || (shift && s.eq_ignore_ascii_case("W"))) =>
                            {
                                let path = state
                                    .previewed_files_path()
                                    .or_else(|| state.cursored_files_path())
                                    .unwrap_or_default();
                                state.docs_open_external(path);
                            }
                            Key::Character(s)
                                if !event.repeat
                                    && (s.as_str() == "O"
                                        || (shift && s.eq_ignore_ascii_case("O"))) =>
                            {
                                let shown = state
                                    .previewed_files_path()
                                    .or_else(|| state.cursored_files_path());
                                state.quarto_open_execute(shown);
                            }
                            Key::Character(s) if !event.repeat && s.as_str() == "e" => {
                                // Concept-annotation edit takes priority when one
                                // is loaded for the cursored node (content is
                                // already in `state.concept`).
                                let mut entered = false;
                                if let (Some(target), Some(info)) =
                                    (state.concept_target_fired.clone(), state.concept.as_ref())
                                {
                                    if info.target == target && info.exists {
                                        // Split out frontmatter so the
                                        // editable buffer holds the body
                                        // only; the header renders
                                        // read-only above the edit area
                                        // and is preserved verbatim on
                                        // save.
                                        let (header, body) = split_frontmatter(&info.content);
                                        state.edit_state = Some(EditState {
                                            target,
                                            expected_ast_hash: info.synced_against.clone(),
                                            header,
                                            original: body.clone(),
                                            buf: EditBuffer::new(body),
                                            confirm_discard: false,
                                            stale_banner: false,
                                            file_node_id: None,
                                            file_version: None,
                                        });
                                        state.rebuild_edit_preview();
                                        entered = true;
                                    }
                                }
                                // Otherwise, if the preview is showing a general
                                // file, edit the file itself: fetch its raw text
                                // via file.read and enter edit mode when the reply
                                // lands (see the FileRead handler). `pending_file_edit`
                                // matches the reply to this request.
                                if !entered && state.edit_state.is_none() {
                                    if let Some(node_id) = state.preview_node_id_fired.clone() {
                                        if node_id.starts_with("files:") {
                                            let ws = state.active_workspace_id.clone();
                                            if let Err(e) =
                                                state.req_tx.send(OutgoingReq::FileRead {
                                                    node_id: node_id.clone(),
                                                    workspace_id: ws,
                                                })
                                            {
                                                tracing::warn!(error = %e, "drop file.read for edit-enter");
                                            } else {
                                                state.pending_file_edit = Some(node_id);
                                            }
                                        }
                                    }
                                }
                            }
                            Key::Named(NamedKey::PageUp) => {
                                // 1/3-pane step preserves reading
                                // context — full-page jumps lost the
                                // user's place. Ctrl+u still half-pages
                                // for the "I really want to jump"
                                // case.
                                let page_step = (h / 3).max(1);
                                let new = (state.preview_scroll as i32 - page_step).max(0);
                                state.preview_scroll = new as u16;
                            }
                            Key::Named(NamedKey::PageDown) => {
                                let page_step = (h / 3).max(1);
                                let new = (state.preview_scroll as i32 + page_step).max(0);
                                state.preview_scroll = new as u16;
                            }
                            // Plain ArrowUp / ArrowDown scroll the
                            // markdown preview vertically by one row.
                            // PNG previews intercept these earlier
                            // (Action::PreviewPngPanUp/Down) so this
                            // arm only fires for non-PNG content.
                            Key::Named(NamedKey::ArrowUp) => {
                                state.preview_scroll = state.preview_scroll.saturating_sub(1);
                            }
                            Key::Named(NamedKey::ArrowDown) => {
                                state.preview_scroll = state.preview_scroll.saturating_add(1);
                            }
                            Key::Named(NamedKey::Home) if !event.repeat => {
                                state.preview_scroll = 0;
                            }
                            Key::Named(NamedKey::End) if !event.repeat => {
                                // Redraw clamps to (total - visible).
                                state.preview_scroll = u16::MAX;
                            }
                            Key::Character(s) if ctrl && s.as_str() == "u" => {
                                let new = (state.preview_scroll as i32 - h / 2).max(0);
                                state.preview_scroll = new as u16;
                            }
                            Key::Character(s) if ctrl && s.as_str() == "d" => {
                                let new = (state.preview_scroll as i32 + h / 2).max(0);
                                state.preview_scroll = new as u16;
                            }
                            _ => {}
                        }
                    }
                    PaneFocus::Llm => {
                        // Forward keystrokes to the backend-side tmux pty.
                        // Esc, Tab, arrows, Ctrl+letter all reach the
                        // terminal so shell editing, tmux prefix
                        // (Ctrl+B), and TUI apps work. To leave this
                        // pane use Ctrl+Arrow — pane move is handled
                        // above before this arm runs.
                        //
                        // Paste shortcut interception: Ctrl+V / Cmd+V /
                        // Shift+Insert read the OS clipboard and forward
                        // as one bracketed-paste blob, so the remote LLM
                        // CLI sees paste-vs-typing correctly and multi-line
                        // text doesn't submit on every embedded newline.
                        // Ctrl+Shift+C: copy the current mouse selection
                        // to the OS clipboard, then consume the key. We
                        // pick this chord (not Ctrl+C) deliberately —
                        // Ctrl+C must still reach the pty as 0x03 so the
                        // LLM CLI's "cancel current request" path works.
                        // No selection? Fall through so a stray
                        // Ctrl+Shift+C still hits the pty.
                        if !event.repeat
                            && ctrl
                            && shift
                            && matches!(&event.logical_key, Key::Character(s) if s.eq_ignore_ascii_case("c"))
                            && state.llm_selection.is_some()
                        {
                            state.copy_llm_selection();
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        let is_paste_shortcut = !event.repeat
                            && match &event.logical_key {
                                Key::Character(s)
                                    if s.eq_ignore_ascii_case("v") && (ctrl || super_) =>
                                {
                                    true
                                }
                                Key::Named(NamedKey::Insert) if shift => true,
                                _ => false,
                            };
                        if is_paste_shortcut {
                            forward_clipboard_paste_to_llm(state);
                            state.last_key = Some(label);
                            state.window.request_redraw();
                            return;
                        }
                        // PgUp/PgDn page the REMOTE pane's scrollback from
                        // the keyboard: tmux owns the ring (our vt100 ring
                        // stays empty under tmux's in-place repaints), so
                        // the backend enters `copy-mode -e` and pages —
                        // exactly what the mouse wheel achieves via SGR
                        // events, minus the mouse. Alternate-screen apps
                        // (vim/less) get the raw key passed through
                        // backend-side so their own paging still works.
                        // Shift+PgUp/PgDn skip this and fall through as raw
                        // bytes — the escape hatch for a remote app that
                        // wants the key itself. Repeats allowed: holding
                        // PgUp keeps paging.
                        if !shift {
                            let scroll = match &event.logical_key {
                                Key::Named(NamedKey::PageUp) => Some(true),
                                Key::Named(NamedKey::PageDown) => Some(false),
                                _ => None,
                            };
                            if let Some(up) = scroll {
                                if let Err(e) =
                                    state.req_tx.send(OutgoingReq::PtyScroll { up })
                                {
                                    tracing::warn!(error = %e, "drop pty.scroll — channel closed");
                                }
                                state.last_key = Some(label);
                                state.window.request_redraw();
                                return;
                            }
                        }
                        let bytes: Option<Vec<u8>> = key_to_pty_bytes(&event.logical_key, ctrl);
                        if let Some(bytes) = bytes {
                            // Any byte we send to the pty snaps the
                            // view back to live so what the user is
                            // typing is always at the bottom of the
                            // LLM pane next to the prompt.
                            state.pty_scroll = 0;
                            if let Err(e) = state.req_tx.send(OutgoingReq::PtyWrite { bytes }) {
                                tracing::warn!(error = %e, "drop pty.write — channel closed");
                            }
                        }
                    }
                }
                state.last_key = Some(label);
                state.window.request_redraw();
            }
            _ => {}
        }
    }

    /// Called by winit after a batch of events is processed, before the loop
    /// goes to sleep. If a frame was deferred by the FRAME_BUDGET cap in
    /// `RedrawRequested`, schedule a wake-up at the next frame boundary so
    /// the deferred draw still lands — just on cadence instead of per-event.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.capture_path.is_some() {
            return;
        }
        // Notify toast expiry: once the sticky window elapses, restore the
        // normal connection status so the toast doesn't linger until the next
        // event. The idle/flash tick below brings us back here within ~1s.
        if let Some(until) = state.notify_sticky_until {
            if std::time::Instant::now() >= until {
                state.notify_sticky_until = None;
                state.rebuild_connection_status();
                state.window.request_redraw();
            }
        }
        if !state.dirty {
            // Idle: nothing animating, but the top-right clock still needs to
            // tick. Schedule a single wake at the next ~1s boundary so the
            // chrome repaints and re-reads `Local::now()`. One wake per second,
            // no busy-loop. `new_events` turns the resume into a redraw.
            //
            // Exception: while a status-change flash is fading, the 1s clock
            // tick is far too coarse for the FLASH_SECS (0.6s) fade — it would
            // jump in one or two steps. Drop to a ~80ms cadence (≈8 frames
            // over the fade, smooth enough to read as a blink) only while a
            // flash is live; `redraw` prunes finished flashes so we fall back
            // to the 1s idle tick automatically once none remain.
            let interval = if state.flash_starts.is_empty() {
                std::time::Duration::from_secs(1)
            } else {
                std::time::Duration::from_millis(80)
            };
            let deadline = std::time::Instant::now() + interval;
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }
        match state.last_frame_at {
            Some(t) => {
                let elapsed = t.elapsed();
                if elapsed >= FRAME_BUDGET {
                    state.dirty = false;
                    state.window.request_redraw();
                } else {
                    let deadline = std::time::Instant::now() + (FRAME_BUDGET - elapsed);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
                }
            }
            None => {
                state.dirty = false;
                state.window.request_redraw();
            }
        }
    }

    /// When the WaitUntil deadline set by `about_to_wait` fires, request the
    /// deferred draw and drop back to Wait so we don't busy-loop.
    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if !matches!(cause, StartCause::ResumeTimeReached { .. }) {
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if state.capture_path.is_some() {
            return;
        }
        // The deadline fired: either a deferred dirty frame is due, or it's the
        // idle clock tick (`!dirty`). Either way request a redraw so the chrome
        // repaints with a fresh `Local::now()`. `about_to_wait` will arm the
        // next 1s wake afterwards, so we don't busy-loop here.
        state.dirty = false;
        state.window.request_redraw();
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

/// Project the REPL scrollback into ratatui `RtLine`s for the BR quadrant.
/// A decoded inline REPL figure: GPU quad + natural pixel dimensions.
struct ReplImage {
    quad: Quad,
    w: u32,
    h: u32,
}

/// One reserved row-region in the REPL scrollback where an inline image
/// paints. `line` is the absolute index into the built line list.
#[derive(Clone, Copy)]
struct ReplImageSlot {
    line: usize,
    rows: u16,
    disp_w: f32,
    disp_h: f32,
    key: (u64, usize),
}

/// One entry contributes a `julia> {code}` header line, then one line per
/// rendered frame (stdout/stderr default+red, value green, error red with
/// dim stack lines). Multi-line frame bodies fan out into one line each so
/// ratatui's word wrap doesn't munge them. In-flight entries get a dim
/// `(running…)` placeholder until the response lands.
///
/// Image frames whose quad is already decoded (`images`) reserve
/// fit-to-width blank rows and report a `ReplImageSlot` — the paint pass
/// overlays the quad there, scissored to the scrollback rect. Frames not
/// yet decoded fall back to a one-line caption for a frame or two.
fn build_repl_lines(
    log: &[ReplEntry],
    images: &std::collections::HashMap<(u64, usize), ReplImage>,
    avail_w_px: f32,
    avail_h_px: f32,
    cell_w: f32,
    cell_h: f32,
) -> (Vec<RtLine<'static>>, Vec<ReplImageSlot>) {
    let mut slots: Vec<ReplImageSlot> = Vec::new();
    let mut out: Vec<RtLine<'static>> = Vec::new();
    for entry in log {
        let (prompt_text, prompt_color) = if entry.pkg_mode {
            ("pkg> ", Color::LightBlue)
        } else {
            ("julia> ", Color::LightCyan)
        };
        // Multi-line submissions (Shift+Enter while typing) get one
        // echo line per segment, first with the prompt and the rest
        // with same-width filler — same convention as the live input.
        let cont_pad: String = " ".repeat(prompt_text.len());
        for (i, seg) in entry.code.split('\n').enumerate() {
            let prefix_span = if i == 0 {
                Span::styled(prompt_text.to_string(), Style::default().fg(prompt_color))
            } else {
                Span::raw(cont_pad.clone())
            };
            out.push(RtLine::from(vec![
                prefix_span,
                Span::styled(seg.to_string(), Style::default()),
            ]));
        }
        // Render whatever frames have streamed in so far — even while the
        // entry is still `in_flight` — so live output (ADR 0009 phase-2
        // streaming) appears tick-by-tick instead of all at once on
        // completion. The `(running…)` / elapsed line is appended AFTER the
        // frames below. (Previously this `continue`d past the frame loop while
        // in_flight, which worked only because the old acceptance ack flipped
        // in_flight=false within milliseconds — that race is now gone.)
        for (frame_idx, frame) in entry.frames.iter().enumerate() {
            match frame {
                ReplFrame::Stdout { text } => {
                    for line in text.lines() {
                        out.push(RtLine::from(line.to_string()));
                    }
                }
                ReplFrame::Stderr { text } => {
                    for line in text.lines() {
                        out.push(RtLine::from(vec![Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::Red),
                        )]));
                    }
                }
                ReplFrame::Value { mime, text } => {
                    // Value frames carry the displayed repr; mime stays
                    // text/plain in the spike. Show in green so it pops
                    // against stdout.
                    let prefix = if mime == "text/plain" {
                        String::new()
                    } else {
                        format!("[{mime}] ")
                    };
                    for (i, line) in text.lines().enumerate() {
                        let s = if i == 0 {
                            format!("{prefix}{line}")
                        } else {
                            line.to_string()
                        };
                        out.push(RtLine::from(vec![Span::styled(
                            s,
                            Style::default().fg(Color::LightGreen),
                        )]));
                    }
                }
                ReplFrame::Error {
                    message,
                    stacktrace,
                } => {
                    for line in message.lines() {
                        out.push(RtLine::from(vec![Span::styled(
                            line.to_string(),
                            Style::default().fg(Color::LightRed),
                        )]));
                    }
                    for sf in stacktrace {
                        out.push(RtLine::from(vec![Span::styled(
                            format!("    at {} ({}:{})", sf.function, sf.file, sf.line),
                            Style::default()
                                .fg(Color::DarkGray)
                                .add_modifier(Modifier::DIM),
                        )]));
                    }
                }
                ReplFrame::Done { .. } => {}
                ReplFrame::Image { mime, bytes, .. } => {
                    let key = (entry.eval_id, frame_idx);
                    // Degenerate width (drawer not yet laid out — the fit
                    // width lags one frame) falls through to the caption
                    // rather than reserving sliver-scaled rows.
                    let sized = (avail_w_px > 4.0 * cell_w)
                        .then(|| images.get(&key))
                        .flatten();
                    if let Some(img) = sized {
                        // Reserve rows for the figure; the paint pass
                        // overlays the quad there. Fit BOTH the drawer's
                        // width and its scrollback height (a figure taller
                        // than the drawer otherwise renders permanently
                        // clipped — first repl-figure capture); never
                        // upscale — small figures render at natural size.
                        let max_w = (avail_w_px - 2.0 * cell_w).max(cell_w);
                        let max_h = (avail_h_px - 3.0 * cell_h).max(cell_h);
                        let scale = (max_w / img.w.max(1) as f32)
                            .min(max_h / img.h.max(1) as f32)
                            .min(1.0);
                        let disp_w = img.w as f32 * scale;
                        let disp_h = img.h as f32 * scale;
                        let rows = ((disp_h / cell_h.max(1.0)).ceil() as u16).max(1);
                        slots.push(ReplImageSlot {
                            line: out.len(),
                            rows,
                            disp_w,
                            disp_h,
                            key,
                        });
                        for _ in 0..rows {
                            out.push(RtLine::from(""));
                        }
                    } else {
                        // Not decoded yet (arrives via the pre-draw pass a
                        // frame later) or decode failed: caption line.
                        out.push(RtLine::from(vec![Span::styled(
                            format!("[image · {mime} · {} bytes]", bytes),
                            Style::default()
                                .fg(Color::LightMagenta)
                                .add_modifier(Modifier::DIM),
                        )]));
                    }
                }
            }
        }
        if entry.in_flight {
            // Still streaming — a dim indicator AFTER the live frames so the
            // user sees output accumulating *and* that more is coming.
            out.push(RtLine::from(vec![Span::styled(
                "(running…)".to_string(),
                Style::default().add_modifier(Modifier::DIM),
            )]));
        } else if entry.elapsed_ms > 0 {
            out.push(RtLine::from(vec![Span::styled(
                format!("  ({} ms)", entry.elapsed_ms),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )]));
        }
    }
    (out, slots)
}

/// Paint a unified box-drawing wireframe over `area`, using `vlines`
/// as the x-positions of inner vertical dividers and `hlines` as the
/// y-positions of inner horizontal dividers (today: at most one — the
/// drawer's top edge). Junctions are computed from the line lists so
/// the wireframe is consistent regardless of how many columns the
/// preset declared; `hlines.is_empty()` means full-height columns,
/// and an empty `vlines` is a single-pane (e.g. maximised) layout.
///
/// In the v1, hlines only ever clip vertical lines (drawer top): the
/// horizontal line crosses the whole width and the verticals stop
/// above it (the drawer below has no inner dividers).
// `drawer_x_end`: when `Some(x)`, the topmost hline (drawer top)
// ends at `x` instead of running to the outer right edge. The cell at
// `x + 1` is `llm_left_vline` (a full-height divider) and gets a ┤
// junction; cells to the right of `x` get the outer vertical run
// unchanged.
// `llm_left_vline`: x of the vertical divider that runs full height
// (Llm's left edge) when the drawer is partially scoped. Other vlines
// stop at the drawer line.
fn draw_wireframe(
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    vlines: &[u16],
    hlines: &[u16],
    drawer_x_end: Option<u16>,
    llm_left_vline: Option<u16>,
    style: Style,
) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let left = area.x;
    let right = area.x + area.width - 1;
    let top = area.y;
    let bot = area.y + area.height - 1;

    let is_vline = |x: u16| vlines.iter().any(|&v| v == x);
    let is_hline = |y: u16| hlines.iter().any(|&h| h == y);
    // The drawer-clip y for *short* vlines. Full-height vlines
    // (llm_left_vline) ignore this and run to bot - 1.
    let short_vline_bot_y = hlines
        .iter()
        .copied()
        .min()
        .map(|h| h.saturating_sub(1))
        .unwrap_or(bot.saturating_sub(1));

    // Outer top/bottom horizontal runs.
    for x in (left + 1)..right {
        if !is_vline(x) {
            buf.set_string(x, top, "─", style);
            buf.set_string(x, bot, "─", style);
        }
    }
    // Outer left/right vertical runs (skip the cell where an hline
    // crosses — junction handling fills it in below).
    for y in (top + 1)..bot {
        if !is_hline(y) {
            buf.set_string(left, y, "│", style);
            buf.set_string(right, y, "│", style);
        }
    }
    // Inner horizontal runs (drawer top). Optionally truncated at
    // `drawer_x_end` when the Llm column wants the right side full
    // height.
    let hline_right_excl = drawer_x_end.map(|x| x + 1).unwrap_or(right);
    for &y in hlines {
        for x in (left + 1)..hline_right_excl {
            if !is_vline(x) {
                buf.set_string(x, y, "─", style);
            }
        }
    }
    // Inner vertical runs. The Llm-left vline runs full height; every
    // other vline stops at the drawer line.
    for &x in vlines {
        let full_height = Some(x) == llm_left_vline;
        let v_bot = if full_height {
            bot.saturating_sub(1)
        } else {
            short_vline_bot_y
        };
        for y in (top + 1)..=v_bot {
            if !is_hline(y) {
                buf.set_string(x, y, "│", style);
            }
        }
    }

    // Outer corners.
    buf.set_string(left, top, "┌", style);
    buf.set_string(right, top, "┐", style);
    buf.set_string(left, bot, "└", style);
    buf.set_string(right, bot, "┘", style);
    // Top-edge T-junctions (vline meets outer top).
    for &x in vlines {
        buf.set_string(x, top, "┬", style);
    }
    // Bottom-edge T-junctions for every vline that reaches the
    // outer bottom — i.e. when no drawer is open, OR when this vline
    // is the Llm-left full-height divider running past the drawer.
    for &x in vlines {
        let full_height = Some(x) == llm_left_vline;
        let reaches_bot = hlines.is_empty() || full_height;
        if reaches_bot {
            buf.set_string(x, bot, "┴", style);
        }
    }
    // Drawer-line endpoints + vline crossings.
    for &y in hlines {
        buf.set_string(left, y, "├", style);
        // Right end of the hline: outer ┤ when the hline spans full
        // width; nothing special at the outer right edge otherwise —
        // the vertical run above continues uninterrupted there.
        if drawer_x_end.is_none() {
            buf.set_string(right, y, "┤", style);
        }
        for &x in vlines {
            let full_height = Some(x) == llm_left_vline;
            if full_height {
                // Hline approaches from the left only; vline runs
                // top → bottom through this cell: ┤.
                buf.set_string(x, y, "┤", style);
            } else {
                // Short vline terminates at the hline from above; no
                // continuation below. Hline crosses both sides: ┴.
                buf.set_string(x, y, "┴", style);
            }
        }
    }
}

/// Flatten a `project.scan` response into TreeView rows. Top-level
/// rows: a synthetic `modules:` root, then per-module rows. Module
/// children: types (with their constructors), then non-constructor
/// functions, then submodules (recursive). Every row carries `file` +
/// `line` on its payload so the cursor-tracking `preview.get` knows
/// which source to fetch and where to focus once line-anchored
/// preview lands.
fn scan_to_tree_rows(modules: &[crate::transport::ScanModule]) -> Vec<TreeRow> {
    let root = TreeNode {
        id: "modules:".to_string(),
        label: "modules".to_string(),
        kind: "modules".to_string(),
        has_children: !modules.is_empty(),
        badges: Vec::new(),
        payload: Default::default(),
    };
    let mut rows = vec![TreeRow {
        node: root,
        depth: 0,
        expanded: !modules.is_empty(),
    }];
    for m in modules {
        emit_scan_module(m, 1, &mut rows, "modules");
    }
    rows
}

fn emit_scan_module(
    m: &crate::transport::ScanModule,
    depth: usize,
    rows: &mut Vec<TreeRow>,
    parent_id: &str,
) {
    let has_children = !m.types.is_empty() || !m.functions.is_empty() || !m.submodules.is_empty();
    let id = format!("{}:{}", parent_id, m.name);
    let mut payload = serde_json::Map::new();
    payload.insert(
        "file".to_string(),
        serde_json::Value::String(m.file.clone()),
    );
    payload.insert("line".to_string(), serde_json::Value::from(m.line));
    rows.push(TreeRow {
        node: TreeNode {
            id: id.clone(),
            label: m.name.clone(),
            kind: "module".to_string(),
            has_children,
            badges: Vec::new(),
            payload,
        },
        depth,
        expanded: has_children,
    });
    for t in &m.types {
        emit_scan_type(t, depth + 1, rows, &id);
    }
    for f in &m.functions {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "file".to_string(),
            serde_json::Value::String(f.file.clone()),
        );
        payload.insert("line".to_string(), serde_json::Value::from(f.line));
        rows.push(TreeRow {
            node: TreeNode {
                id: format!("{}:fn:{}:{}", id, f.name, f.line),
                label: f.name.clone(),
                kind: if f.kind.is_empty() {
                    "function".to_string()
                } else {
                    f.kind.clone()
                },
                has_children: false,
                badges: Vec::new(),
                payload,
            },
            depth: depth + 1,
            expanded: false,
        });
    }
    for sm in &m.submodules {
        emit_scan_module(sm, depth + 1, rows, &id);
    }
}

fn emit_scan_type(
    t: &crate::transport::ScanType,
    depth: usize,
    rows: &mut Vec<TreeRow>,
    parent_id: &str,
) {
    let has_children = !t.constructors.is_empty();
    let tid = format!("{}:type:{}", parent_id, t.name);
    let mut payload = serde_json::Map::new();
    payload.insert(
        "file".to_string(),
        serde_json::Value::String(t.file.clone()),
    );
    payload.insert("line".to_string(), serde_json::Value::from(t.line));
    let label = if t.kind == "struct" || t.kind.is_empty() {
        t.name.clone()
    } else {
        // "Animal (abstract)" reads naturally next to plain struct rows.
        format!("{} ({})", t.name, t.kind)
    };
    rows.push(TreeRow {
        node: TreeNode {
            id: tid.clone(),
            label,
            kind: if t.kind.is_empty() {
                "struct".to_string()
            } else {
                t.kind.clone()
            },
            has_children,
            badges: Vec::new(),
            payload,
        },
        depth,
        expanded: has_children,
    });
    for c in &t.constructors {
        let mut payload = serde_json::Map::new();
        payload.insert(
            "file".to_string(),
            serde_json::Value::String(c.file.clone()),
        );
        payload.insert("line".to_string(), serde_json::Value::from(c.line));
        rows.push(TreeRow {
            node: TreeNode {
                id: format!("{}:ctor:{}", tid, c.line),
                label: format!("{}(…)", c.name),
                kind: "constructor".to_string(),
                has_children: false,
                badges: Vec::new(),
                payload,
            },
            depth: depth + 1,
            expanded: false,
        });
    }
}

/// Paint the vt100 terminal grid into `rect`. Each cell of the
/// emulator screen at (row, col) is written at (rect.x + col,
/// rect.y + row) with the cell's foreground colour and bold/italic
/// attributes mapped onto ratatui Style. Background colour is
/// dropped for now — the chrome pipeline doesn't carry it yet
/// (planned bg colour + underline modifiers).
/// Translate a key event into the byte sequence a PTY expects. Shared by
/// the LLM pane (remote tmux pty) and the local terminal drawer (G4). With
/// `ctrl`, ASCII letters map to control codes (Ctrl+C → 0x03, Ctrl+B →
/// 0x02, …) so shell editing, signals, and tmux prefixes work; non-letters
/// under Ctrl pass through verbatim. Returns `None` for keys with no PTY
/// encoding (bare modifiers, etc.).
fn key_to_pty_bytes(key: &Key, ctrl: bool) -> Option<Vec<u8>> {
    match key {
        Key::Named(NamedKey::Enter) => Some(b"\r".to_vec()),
        Key::Named(NamedKey::Backspace) => Some(b"\x7f".to_vec()),
        Key::Named(NamedKey::Tab) => Some(b"\t".to_vec()),
        Key::Named(NamedKey::Escape) => Some(b"\x1b".to_vec()),
        Key::Named(NamedKey::Space) => Some(b" ".to_vec()),
        Key::Named(NamedKey::ArrowUp) => Some(b"\x1b[A".to_vec()),
        Key::Named(NamedKey::ArrowDown) => Some(b"\x1b[B".to_vec()),
        Key::Named(NamedKey::ArrowRight) => Some(b"\x1b[C".to_vec()),
        Key::Named(NamedKey::ArrowLeft) => Some(b"\x1b[D".to_vec()),
        Key::Named(NamedKey::Home) => Some(b"\x1b[H".to_vec()),
        Key::Named(NamedKey::End) => Some(b"\x1b[F".to_vec()),
        Key::Named(NamedKey::PageUp) => Some(b"\x1b[5~".to_vec()),
        Key::Named(NamedKey::PageDown) => Some(b"\x1b[6~".to_vec()),
        Key::Named(NamedKey::Delete) => Some(b"\x1b[3~".to_vec()),
        Key::Character(s) => {
            if ctrl {
                let mut out = Vec::with_capacity(s.len());
                for c in s.chars() {
                    let lower = c.to_ascii_lowercase();
                    if lower.is_ascii_lowercase() {
                        out.push((lower as u8) - b'a' + 1);
                    } else {
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                }
                if out.is_empty() {
                    None
                } else {
                    Some(out)
                }
            } else {
                Some(s.as_bytes().to_vec())
            }
        }
        _ => None,
    }
}

fn paint_terminal(
    buf: &mut ratatui::buffer::Buffer,
    rect: ratatui::layout::Rect,
    screen: &vt100::Screen,
) {
    let cols = rect.width;
    let rows = rect.height;
    // vt100 reports cursor in row-major (row, col) of its own grid; we
    // overlay it via Modifier::REVERSED below. Captured before the cell
    // walk so the cursor cell's pre-existing style still wins for color.
    let (cur_row, cur_col) = screen.cursor_position();
    let cursor_hidden = screen.hide_cursor();
    for row in 0..rows {
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            let contents = cell.contents();
            // Empty cell: nothing to draw — the wireframe / surrounding
            // paint already cleared this area.
            let glyph = if contents.is_empty() { " " } else { contents };
            let mut fg = vt100_color_to_ratatui(cell.fgcolor());
            // Dim on the default foreground would otherwise fall back to
            // the chrome's white default — the Claude Code CLI uses dim
            // to muff out its suggested-prompt placeholder, so promote
            // it to Gray so the muting actually shows.
            if cell.dim() && matches!(fg, Color::Reset) {
                fg = Color::Gray;
            }
            let mut style = Style::default().fg(fg);
            if cell.bold() {
                style = style.add_modifier(Modifier::BOLD);
            }
            if cell.dim() {
                style = style.add_modifier(Modifier::DIM);
            }
            if cell.italic() {
                style = style.add_modifier(Modifier::ITALIC);
            }
            // Block cursor XOR's REVERSED with the cell's existing
            // inverse state — so a cursor sitting on an inverse status-bar
            // cell flips back to non-reversed instead of disappearing,
            // matching how xterm/alacritty draw their block cursors.
            // Selection visibility lives on the GPU side as a yellow
            // quad rendered before text, not in this REVERSED flag —
            // REVERSED inverted both fg and bg, which made selected text
            // hard to read.
            let is_cursor = !cursor_hidden && row == cur_row && col == cur_col;
            let reverse = cell.inverse() ^ is_cursor;
            if reverse {
                style = style.add_modifier(Modifier::REVERSED);
            }
            buf.set_string(rect.x + col, rect.y + row, glyph, style);
        }
    }
}

/// Map a vt100 fg/bg colour to the nearest ratatui Color. ANSI 16-colour
/// palette goes through the named variants; indexed (256-colour) and
/// RGB pass through as Color::Indexed / Color::Rgb so the existing
/// chrome render path can emit them.
fn vt100_color_to_ratatui(c: vt100::Color) -> Color {
    use vt100::Color as V;
    match c {
        V::Default => Color::Reset,
        V::Idx(0) => Color::Black,
        V::Idx(1) => Color::Red,
        V::Idx(2) => Color::Green,
        V::Idx(3) => Color::Yellow,
        V::Idx(4) => Color::Blue,
        V::Idx(5) => Color::Magenta,
        V::Idx(6) => Color::Cyan,
        V::Idx(7) => Color::Gray,
        V::Idx(8) => Color::DarkGray,
        V::Idx(9) => Color::LightRed,
        V::Idx(10) => Color::LightGreen,
        V::Idx(11) => Color::LightYellow,
        V::Idx(12) => Color::LightBlue,
        V::Idx(13) => Color::LightMagenta,
        V::Idx(14) => Color::LightCyan,
        V::Idx(15) => Color::White,
        V::Idx(other) => Color::Indexed(other),
        V::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Overlay a pane title onto a border row, clamped to `max_w` cells so
/// it can't overrun the pane's width and clobber the inner-cross
/// junction or the neighbouring pane's title. The wireframe was
/// painted first with `─` everywhere on the edge; this writes the
/// title text starting at `(x, y)` with the given style, replacing
/// those cells. Title strings carry their own leading/trailing space
/// so the line breaks cleanly on either side of the label.
/// Middle-truncate `s` to at most `max` chars, biasing the tail so a path's
/// basename + extension stay visible (`src/very/long/Mod…Name.jl`). Returns
/// `s` unchanged when it already fits. The full name is always recoverable
/// via Ctrl+C in NavTree (copies the absolute path).
fn middle_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return chars.iter().take(max).collect();
    }
    let keep = max - 1; // one cell for the ellipsis
    let head = keep / 2; // favour the tail so the extension survives
    let tail = keep - head;
    let head_s: String = chars.iter().take(head).collect();
    let tail_s: String = chars.iter().skip(chars.len() - tail).collect();
    format!("{head_s}…{tail_s}")
}

/// Query the OS for the primary battery's charge level and return a short
/// chrome label like `85%`, or `+85%` while charging (the leading `+` marks a
/// charging state). Returns `None` when there's no battery (desktop / CI) or
/// the query errors — the caller renders nothing in that case, never a fake
/// `0%`/`N/A`. Cross-platform via the `battery` (starship-battery) crate; no
/// OS-specific calls here.
///
/// This reads the OS and is not free; callers must rate-limit it (see
/// `State::refresh_battery_label`), not call it per frame.
fn query_battery_label() -> Option<String> {
    use battery::State as BatState;

    let manager = battery::Manager::new().ok()?;
    // First battery only. `batteries()` can error; an empty iterator means no
    // battery present — both yield `None` (paint nothing).
    let battery = manager.batteries().ok()?.next()?.ok()?;

    // State of charge is a ratio in [0, 1]; render as a whole percent.
    let ratio = battery.state_of_charge().value;
    if !ratio.is_finite() {
        return None;
    }
    let pct = (ratio * 100.0).round().clamp(0.0, 100.0) as u32;

    let charging = matches!(battery.state(), BatState::Charging);
    Some(if charging {
        format!("+{pct}%")
    } else {
        format!("{pct}%")
    })
}

fn write_title(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    title: &str,
    max_w: u16,
    style: Style,
) {
    if y >= buf.area.height {
        return;
    }
    let cap = (max_w as usize).min(buf.area.width.saturating_sub(x) as usize);
    let s: String = title.chars().take(cap).collect();
    buf.set_string(x, y, &s, style);
}

/// Scale an RGB colour's brightness by `f` (saturating). `f > 1.0`
/// brightens (toward white-ish, channel-clamped); `f < 1.0` dims. Used by
/// the selected-session contrast levers so a single multiplier expresses
/// both "pop the selection brighter" and "fade the non-selected".
fn scale_rgb(rgb: (u8, u8, u8), f: f32) -> (u8, u8, u8) {
    let s = |c: u8| -> u8 { ((c as f32 * f).round()).clamp(0.0, 255.0) as u8 };
    (s(rgb.0), s(rgb.1), s(rgb.2))
}

/// Brightness multiplier applied to a *non-selected* session name under the
/// "dim" contrast lever, so the selected name pops by contrast. Distinct
/// from (stronger than) the existing wilt/idle ~0.65 dim.
const CONTRAST_DIM_FACTOR: f32 = 0.55;
/// Brightness multiplier applied to the *selected* coloured session name
/// under the "bright" lever, so the active row's tone reads clearly brighter
/// than non-selected coloured rows (which keep their full tone).
const CONTRAST_BRIGHT_FACTOR: f32 = 1.35;

/// Resolve a coloured (tone-bearing) state-nav session name to its final
/// `(rgb, bold, dim)` under the active contrast lever — shared by the
/// Sessions-mode nav rows and the bottom strip so the two stay pixel-aligned.
/// `wilted` is the stale-"working" flag (forces a dim). `is_active` is the
/// selected/active row. `contrast_dim` is the `--contrast-mode dim` lever.
///
/// bright lever: the active coloured row keeps its tone hue but is scaled up
/// (`CONTRAST_BRIGHT_FACTOR`) so it out-reads non-active coloured rows; bold.
/// dim lever: non-active coloured rows are scaled down
/// (`CONTRAST_DIM_FACTOR`); the active row keeps its full tone + bold. Either
/// way the tone hue + the bold-composes-with-colour behaviour is preserved.
///
/// `flash_f` (0..1, the `flash_factor`) is composed *last*: the resolved
/// colour is lerped toward white by `flash_f` so a just-changed name blinks
/// bright then fades back to its contrast-adjusted tone.
fn contrast_tone_rgb(
    tone: AgentTone,
    wilted: bool,
    is_active: bool,
    contrast_dim: bool,
    flash_f: f32,
) -> (Option<(u8, u8, u8)>, bool, bool) {
    let base = tone.rgb();
    let rgb = match (is_active, contrast_dim) {
        // bright lever, active coloured row → push the tone brighter.
        (true, false) => base.map(|c| scale_rgb(c, CONTRAST_BRIGHT_FACTOR)),
        // dim lever, non-active coloured row → fade it back.
        (false, true) => base.map(|c| scale_rgb(c, CONTRAST_DIM_FACTOR)),
        _ => base,
    };
    // Flash composes on top of the contrast result.
    let rgb = if flash_f > 0.0 {
        rgb.map(|c| lerp_to_white(c, flash_f))
    } else {
        rgb
    };
    (rgb, is_active, wilted)
}

/// Agent work-state tone for the state-nav Sessions render (ADR 0023). Maps
/// the registry `agent_state` to a row colour identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AgentTone {
    Working,
    Idle,
    Waiting,
    Blocked,
    Done,
}

impl AgentTone {
    /// Base row colour, pinned to the Julia brand palette so the session
    /// tones read as one coherent logo+session palette (maintainer directive via
    /// sot-docs) — all 4 Julia brand colours + gray, no yellow:
    ///   working = Julia green  #389826 (live),
    ///   waiting = Julia purple #9558B2 (delegated to a long job; idle-of-own-
    ///             work but not free),
    ///   blocked = Julia red    #CB3C33 (needs attention — the loud one),
    ///   done    = Julia blue   #4063D8 (finished),
    ///   idle    = gray (quiet; no Julia equiv).
    /// `rgb()` routes these through `chrome::ratatui_color_to_rgb`, which passes
    /// `Color::Rgb` straight through, so the nav rows and bottom strip pin to
    /// the exact hex.
    fn color(self) -> Color {
        match self {
            AgentTone::Working => Color::Rgb(56, 152, 38), // Julia green  #389826
            AgentTone::Idle => Color::Gray,
            AgentTone::Waiting => Color::Rgb(149, 88, 178), // Julia purple #9558B2
            AgentTone::Blocked => Color::Rgb(203, 60, 51),  // Julia red    #CB3C33
            AgentTone::Done => Color::Rgb(64, 99, 216),     // Julia blue   #4063D8
        }
    }

    /// Same tone as raw RGB, routed through the chrome's ratatui→RGB table so
    /// the bottom session strip (which draws raw `text::Line` colours, not
    /// ratatui styles) matches the Sessions-mode nav rows exactly.
    fn rgb(self) -> Option<(u8, u8, u8)> {
        crate::chrome::ratatui_color_to_rgb(Some(self.color()))
    }
}

/// Minutes after which a still-"working" agent that hasn't re-stamped its
/// status is treated as stale and wilted (dimmed). Only "working" ages —
/// idle/waiting/done are resting states, and a long "blocked" stays loud on
/// purpose.
const AGENT_STALE_MINUTES: i64 = 10;

/// Duration of the status-change flash (ADR 0023): when a session's
/// work-state changes, its name brightens toward white and fades to its
/// resting tone over this window. Short enough to read as a blink, long
/// enough to catch the eye. Shared by the nav rows + the bottom strip.
const FLASH_SECS: f32 = 0.6;

/// How long a pushed `notify` toast stays pinned on the status line — long
/// enough to read across a workspace switch (which rebuilds the status
/// immediately and would otherwise clobber it). After this window
/// `about_to_wait` restores the normal connection status on the idle tick.
// 10s (was 4s): fe-command notifies render only on the one-line status bar
// today and were reliably missed at 4s (2026-07-10 papers-geometry diagnosis).
// A real toast surface is queued (ops TODO, rides the R6 redraw decomposition).
const NOTIFY_STICKY: std::time::Duration = std::time::Duration::from_secs(10);

/// Flash brightness factor for a name whose state changed `elapsed` ago:
/// 1.0 right at the transition, ramping linearly to 0.0 at `FLASH_SECS`,
/// clamped outside the window. Callers lerp the name colour toward white by
/// this factor. Pulled out so the ramp stays unit-testable.
fn flash_factor(elapsed_secs: f32) -> f32 {
    (1.0 - elapsed_secs / FLASH_SECS).clamp(0.0, 1.0)
}

/// Lerp an RGB colour toward white by `t` (0.0 = unchanged, 1.0 = full
/// white). Used to brighten a flashing session name; `t` is the
/// `flash_factor`. Saturating cast keeps it within `u8`.
fn lerp_to_white(rgb: (u8, u8, u8), t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let lerp = |c: u8| -> u8 { (c as f32 + (255.0 - c as f32) * t).round() as u8 };
    (lerp(rgb.0), lerp(rgb.1), lerp(rgb.2))
}

/// Apply a flash to a plain (non-tone) strip name with a concrete base
/// colour: lerp toward white by `flash` when it's live, else leave the base
/// untouched. Keeps the strip's plain-branch flash composition in one place.
fn flash_plain(flash: f32, base: (u8, u8, u8)) -> (u8, u8, u8) {
    if flash > 0.0 {
        lerp_to_white(base, flash)
    } else {
        base
    }
}

/// Resolve a Sessions row's agent state from its node payload into the render
/// tone plus a wilt flag (true = active state gone stale). `None` when there
/// is no agent state to show, so the row renders exactly as it did before
/// state-nav. `now` is injected so the staleness check stays unit-testable.
/// Built-in monitor-width tier for the startup font-scale SEED — used only
/// when no per-host persisted zoom and no `[font] scale` settings key exist.
/// Wide displays read better a notch larger (maintainer note, 2026-07-03: 1.1 on a
/// 4096×1728 @ 96 DPI ultrawide; 3440 catches the common ultrawide widths).
/// Physical pixels, pre-DPR — DPR scaling is already applied separately.
fn default_font_scale_for_width(width_px: u32) -> f32 {
    if width_px >= 3440 {
        1.1
    } else {
        1.0
    }
}

fn agent_tone_for(
    payload: &serde_json::Map<String, serde_json::Value>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(AgentTone, bool)> {
    let state = payload
        .get("agent_state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let status_at = payload
        .get("agent_status_at")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    agent_tone_from(state, status_at, now)
}

/// Core state→tone + staleness logic on the raw registry fields, shared by the
/// Sessions-mode rows (`agent_tone_for`, payload-keyed) and the bottom session
/// strip (slug-keyed `workspace_states`). `None` for an empty/unknown state so
/// the caller renders exactly as it did before state-nav. `now` is injected so
/// the wilt check stays unit-testable.
fn agent_tone_from(
    state: &str,
    status_at: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(AgentTone, bool)> {
    let tone = match state {
        "working" => AgentTone::Working,
        "idle" => AgentTone::Idle,
        "waiting" => AgentTone::Waiting,
        "blocked" => AgentTone::Blocked,
        "done" => AgentTone::Done,
        _ => return None,
    };
    let aged = tone == AgentTone::Working
        && (!status_at.is_empty())
            .then(|| chrono::DateTime::parse_from_rfc3339(status_at).ok())
            .flatten()
            .map(|t| {
                now.signed_duration_since(t.with_timezone(&chrono::Utc))
                    > chrono::Duration::minutes(AGENT_STALE_MINUTES)
            })
            .unwrap_or(false);
    Some((tone, aged))
}

/// Render one TreeRow as a single chrome line: cursor caret, depth indent,
/// disclosure char, label, optional pin sigil. Width is fixed-cell ASCII so
/// the per-row run-length stays predictable for the chrome backend's text
/// projection. `pinned` adds a ` ★` suffix so the user can spot the pinned
/// node even when the cursor is elsewhere; the cell-count stays ASCII so
/// no chrome layout math breaks.
fn format_tree_row(row: &TreeRow, selected: bool, pinned: bool) -> String {
    let caret = if selected { '>' } else { ' ' };
    let disclosure = if row.node.has_children {
        if row.expanded {
            'v'
        } else {
            '>'
        }
    } else {
        '.'
    };
    let indent = "  ".repeat(row.depth);
    let suffix = if pinned { " *" } else { "" };
    format!("{caret} {indent}{disclosure} {}{suffix}", row.node.label)
}

/// Short human-readable name for a winit logical key — what to surface in the
/// chrome and in tracing logs. Falls back to a `Debug` print for keys we
/// haven't pattern-matched yet.
fn key_label(k: &Key) -> String {
    match k {
        Key::Character(s) => s.to_string(),
        Key::Named(n) => match n {
            NamedKey::Enter => "Enter".to_string(),
            NamedKey::Tab => "Tab".to_string(),
            NamedKey::Space => "Space".to_string(),
            NamedKey::Backspace => "Backspace".to_string(),
            NamedKey::Escape => "Escape".to_string(),
            NamedKey::ArrowUp => "Up".to_string(),
            NamedKey::ArrowDown => "Down".to_string(),
            NamedKey::ArrowLeft => "Left".to_string(),
            NamedKey::ArrowRight => "Right".to_string(),
            other => format!("{other:?}"),
        },
        other => format!("{other:?}"),
    }
}

/// Walk one step backward (older) through the REPL history. Returns
/// the code of the entry now selected, or `None` if there's nothing
/// older to step to. On the first step of a walk the current
/// `input` is saved into `saved` so a later forward-walk past the
/// newest entry can restore it.
///
/// In-flight entries are skipped — replaying a still-running line
/// would be confusing, and an `in_flight=true` entry is usually the
/// one the user just submitted anyway.
fn history_step_back(
    log: &[ReplEntry],
    pos: &mut Option<usize>,
    saved: &mut Option<String>,
    input: &str,
) -> Option<String> {
    let candidates: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.in_flight)
        .map(|(i, _)| i)
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let new_pos = match *pos {
        None => {
            *saved = Some(input.to_string());
            candidates.len() - 1
        }
        Some(0) => return None,
        Some(p) => p - 1,
    };
    *pos = Some(new_pos);
    Some(log[candidates[new_pos]].code.clone())
}

/// Walk one step forward (newer). Returns the code of the entry now
/// selected, or — when walking past the newest entry — the saved
/// in-progress buffer (which also exits the walk by clearing `pos`
/// and `saved`). Returns `None` when not currently walking.
fn history_step_forward(
    log: &[ReplEntry],
    pos: &mut Option<usize>,
    saved: &mut Option<String>,
) -> Option<String> {
    let p = (*pos)?;
    let candidates: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.in_flight)
        .map(|(i, _)| i)
        .collect();
    if p + 1 >= candidates.len() {
        let restored = saved.take().unwrap_or_default();
        *pos = None;
        return Some(restored);
    }
    let new_pos = p + 1;
    *pos = Some(new_pos);
    Some(log[candidates[new_pos]].code.clone())
}

/// Grant the next process the right to take the OS foreground (Windows only).
///
/// Called by the outgoing FE just before it exits 75 for an ADR-0017
/// self-relaunch. Because this process currently owns the foreground, it is
/// permitted to call `AllowSetForegroundWindow(ASFW_ANY)`, which lifts the
/// foreground lock so the *next* `SetForegroundWindow` from any process is
/// honoured. The relaunched FE issues that call on its first paint
/// (`force_os_foreground`), so the new window comes up focused instead of
/// merely flashing the taskbar. The grant lasts until the next user input,
/// which comfortably covers the restage+respawn down-window.
#[cfg(windows)]
fn allow_next_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

/// Force the window to the OS foreground (Windows only). Returns `true` once
/// our window actually holds the foreground.
///
/// winit's `focus_window()` issues `SetForegroundWindow`, which Windows
/// silently refuses for a process that isn't already the foreground
/// process (the foreground lock) — it just flashes the taskbar instead.
/// After an ADR-0017 self-relaunch the freshly-spawned FE is precisely
/// that: a brand-new process the user hasn't clicked, spawned `-WindowStyle
/// Hidden` from the background supervisor, so the relaunched window comes up
/// behind whatever took foreground during the down-window.
///
/// Escalating sequence, each step covering a case the prior misses:
///  1. `AttachThreadInput` to the current foreground thread, so the OS treats
///     our `SetForegroundWindow` as same-input-queue and honours it.
///  2. `SetWindowPos` HWND_TOPMOST→HWND_NOTOPMOST toggle to force the window
///     to the top of the z-order while attached.
///  3. If still not foreground, `ShowWindow` SW_MINIMIZE→SW_RESTORE — the one
///     transition Windows always lets take the foreground (a brief flicker,
///     but only on the fallback path). No-op on non-Windows targets.
#[cfg(windows)]
fn force_os_foreground(window: &winit::window::Window) -> bool {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        SetWindowPos, ShowWindow, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE,
        SW_MINIMIZE, SW_RESTORE, SW_SHOW,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(h) = handle.as_raw() else {
        return false;
    };
    let hwnd = h.hwnd.get() as HWND;

    unsafe {
        if GetForegroundWindow() == hwnd {
            return true; // already foreground — nothing to do
        }
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let our_thread = GetCurrentThreadId();
        // Only attach when the foreground belongs to another thread; attaching
        // a thread to itself fails and isn't needed.
        let attached = !fg.is_null()
            && fg_thread != 0
            && fg_thread != our_thread
            && AttachThreadInput(our_thread, fg_thread, 1) != 0;
        ShowWindow(hwnd, SW_SHOW);
        // Toggle topmost to reorder above everything, then drop back so the
        // window doesn't stay pinned over other apps.
        SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        SetWindowPos(hwnd, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        SetActiveWindow(hwnd);
        SetFocus(hwnd);
        if attached {
            AttachThreadInput(our_thread, fg_thread, 0);
        }
        if GetForegroundWindow() == hwnd {
            tracing::info!(
                attached,
                "force_os_foreground: took foreground via attach/topmost"
            );
            return true;
        }
        // Hard fallback: minimize→restore is the transition Windows always
        // grants the foreground to. Only reached when the above failed.
        tracing::info!(
            attached,
            "force_os_foreground: attach/topmost failed; trying minimize/restore"
        );
        ShowWindow(hwnd, SW_MINIMIZE);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
        let ok = GetForegroundWindow() == hwnd;
        tracing::info!(ok, "force_os_foreground: minimize/restore result");
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sot_protocol::TreeNode;

    // State-nav (ADR 0023): agent work-state tone + staleness aging.
    fn agent_payload(state: &str, status_at: &str) -> serde_json::Map<String, serde_json::Value> {
        let mut p = serde_json::Map::new();
        p.insert(
            "agent_state".into(),
            serde_json::Value::String(state.into()),
        );
        p.insert(
            "agent_status_at".into(),
            serde_json::Value::String(status_at.into()),
        );
        p
    }

    #[test]
    fn agent_tone_maps_states_and_absence() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T18:40:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let fresh = "2026-06-15T18:39:00Z"; // 1 min ago
        assert_eq!(
            agent_tone_for(&agent_payload("working", fresh), now),
            Some((AgentTone::Working, false))
        );
        assert_eq!(
            agent_tone_for(&agent_payload("idle", fresh), now),
            Some((AgentTone::Idle, false))
        );
        assert_eq!(
            agent_tone_for(&agent_payload("blocked", fresh), now),
            Some((AgentTone::Blocked, false))
        );
        assert_eq!(
            agent_tone_for(&agent_payload("done", fresh), now),
            Some((AgentTone::Done, false))
        );
        // No state, unknown state, and a wholly empty payload → render as a
        // normal row (no tone).
        assert_eq!(agent_tone_for(&agent_payload("", fresh), now), None);
        assert_eq!(agent_tone_for(&agent_payload("spinning", fresh), now), None);
        assert_eq!(agent_tone_for(&serde_json::Map::new(), now), None);
    }

    #[test]
    fn agent_working_wilts_when_stale_others_do_not() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-06-15T19:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let old = "2026-06-15T18:40:00Z"; // 20 min ago > AGENT_STALE_MINUTES
                                          // A working agent that hasn't re-stamped in >10 min wilts.
        assert_eq!(
            agent_tone_for(&agent_payload("working", old), now),
            Some((AgentTone::Working, true))
        );
        // Idle/done/blocked never wilt — only "working" ages.
        assert_eq!(
            agent_tone_for(&agent_payload("idle", old), now),
            Some((AgentTone::Idle, false))
        );
        assert_eq!(
            agent_tone_for(&agent_payload("blocked", old), now),
            Some((AgentTone::Blocked, false))
        );
        // A working agent with a missing/garbage timestamp is treated as
        // fresh (don't wilt on a parse failure).
        assert_eq!(
            agent_tone_for(&agent_payload("working", ""), now),
            Some((AgentTone::Working, false))
        );
        assert_eq!(
            agent_tone_for(&agent_payload("working", "not-a-date"), now),
            Some((AgentTone::Working, false))
        );
    }

    // ADR 0022: source-pixel ROI mapping.
    #[test]
    fn roi_fit_is_full_image() {
        // Canvas exactly fills the pane (zoom 1, pane-aspect image): whole image.
        let r = visible_roi_px(0.0, 0.0, 800.0, 600.0, 0.0, 0.0, 800.0, 600.0, 800, 600);
        assert_eq!(r, Some((0, 0, 800, 600)));
    }

    #[test]
    fn roi_centered_zoom_is_center_quarter() {
        // 2× canvas centered on the pane → the central half in each axis is
        // visible = source [200..600) × [150..450) for an 800×600 image.
        let (cw, ch) = (1600.0, 1200.0);
        let (px, py, pw, ph) = (0.0, 0.0, 800.0, 600.0);
        let (cx, cy) = (px + pw * 0.5 - cw * 0.5, py + ph * 0.5 - ch * 0.5); // centered
        let r = visible_roi_px(cx, cy, cw, ch, px, py, pw, ph, 800, 600).unwrap();
        assert_eq!(r, (200, 150, 400, 300));
    }

    #[test]
    fn roi_pan_to_top_left_corner() {
        // 2× canvas panned so its top-left aligns with the pane origin → the
        // top-left quarter of the source is visible.
        let (cw, ch) = (1600.0, 1200.0);
        let r = visible_roi_px(0.0, 0.0, cw, ch, 0.0, 0.0, 800.0, 600.0, 800, 600).unwrap();
        assert_eq!(r, (0, 0, 400, 300));
    }

    #[test]
    fn roi_none_when_offscreen_or_degenerate() {
        // Canvas entirely left of the pane → nothing visible.
        assert_eq!(
            visible_roi_px(-2000.0, 0.0, 800.0, 600.0, 0.0, 0.0, 800.0, 600.0, 800, 600),
            None
        );
        // Zero source dims → None.
        assert_eq!(
            visible_roi_px(0.0, 0.0, 800.0, 600.0, 0.0, 0.0, 800.0, 600.0, 0, 600),
            None
        );
    }

    #[test]
    fn is_image_node_id_matches_rasters_not_pdf() {
        assert!(State::is_image_node_id("files:plots/a.png"));
        assert!(State::is_image_node_id("files:IMG.JPEG"));
        assert!(!State::is_image_node_id("files:doc.pdf"));
        assert!(!State::is_image_node_id("files:src/lib.jl"));
    }

    fn node(id: &str, label: &str, has_children: bool) -> TreeNode {
        TreeNode {
            id: id.to_string(),
            label: label.to_string(),
            kind: "files".to_string(),
            has_children,
            badges: Vec::new(),
            payload: Default::default(),
        }
    }

    #[test]
    fn detects_running_claude_in_pane() {
        // Idle footer.
        assert!(pane_shows_running_claude(
            "│ > Try \"edit\"                          │\n  ? for shortcuts"
        ));
        // Working footer.
        assert!(pane_shows_running_claude("✶ Pondering… (esc to interrupt)"));
        // --dangerously-skip-permissions banner.
        assert!(pane_shows_running_claude("  Bypassing Permissions"));
        // Welcome box (just-booted).
        assert!(pane_shows_running_claude("✻ Welcome to Claude Code!"));
        // Real frameconn steady-state footer (captured 2026-06-02) —
        // a claude WITHOUT the bypass flag still shows the agents hint.
        assert!(pane_shows_running_claude(
            "⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents"
        ));
        assert!(pane_shows_running_claude(
            "                    ← for agents"
        ));
    }

    #[test]
    fn no_false_positive_on_shell_prompt() {
        // A freshly cd'd shell pane — must NOT look like a running claude,
        // else we'd never autostart a genuinely fresh agent.
        assert!(!pane_shows_running_claude("user@host:~/dev/some-project$ "));
        assert!(!pane_shows_running_claude(""));
    }

    #[test]
    fn pane_command_claude_guard() {
        // Authoritative backend probe: `claude` is exactly what
        // `pane_current_command` reports for a live agent; `node` is the runtime
        // it may exec under. Both must
        // suppress the autostart launch.
        assert!(pane_command_is_claude(Some("claude")));
        assert!(pane_command_is_claude(Some("node")));
        assert!(pane_command_is_claude(Some(" claude\n"))); // tmux can pad/newline
        assert!(pane_command_is_claude(Some("CLAUDE")));
        // A shell prompt or unknown foreground means "not confirmed running" —
        // fall through to the screen-scrape scan, then launch a real dead agent.
        assert!(!pane_command_is_claude(Some("bash")));
        assert!(!pane_command_is_claude(Some("zsh")));
        assert!(!pane_command_is_claude(Some("")));
        // No probe (same-target resize, or backend predating the field).
        assert!(!pane_command_is_claude(None));
    }

    #[test]
    fn drawer_toggle_is_symmetric() {
        use DrawerContent::*;
        // Open from closed.
        assert_eq!(Closed.toggle(Repl), Repl);
        assert_eq!(Closed.toggle(Terminal), Terminal);
        // Same key closes.
        assert_eq!(Repl.toggle(Repl), Closed);
        assert_eq!(Terminal.toggle(Terminal), Closed);
        // Other key swaps content without closing.
        assert_eq!(Repl.toggle(Terminal), Terminal);
        assert_eq!(Terminal.toggle(Repl), Repl);
    }

    #[test]
    fn drawer_is_open_only_when_not_closed() {
        assert!(!DrawerContent::Closed.is_open());
        assert!(DrawerContent::Repl.is_open());
        assert!(DrawerContent::Terminal.is_open());
    }

    #[test]
    fn middle_truncate_keeps_short_strings() {
        assert_eq!(middle_truncate("short.jl", 20), "short.jl");
        assert_eq!(middle_truncate("exact", 5), "exact");
    }

    #[test]
    fn middle_truncate_preserves_extension_in_tail() {
        let out = middle_truncate("src/very/long/path/MyLongModuleName.jl", 18);
        assert_eq!(out.chars().count(), 18);
        assert!(out.contains('…'));
        // tail-biased: the extension survives the cut
        assert!(out.ends_with(".jl"), "got {out:?}");
        assert!(out.starts_with("src/"), "got {out:?}");
    }

    #[test]
    fn middle_truncate_degenerate_widths() {
        assert_eq!(middle_truncate("abcdef", 3), "abc");
        assert_eq!(middle_truncate("abcdef", 1), "a");
        assert_eq!(middle_truncate("abcdef", 0), "");
    }

    #[test]
    fn looks_binary_detects_nul_not_text() {
        assert!(!looks_binary(b"plain text source\nfn main() {}\n"));
        assert!(!looks_binary(b""));
        // HDF5-ish: signature has a NUL very early.
        assert!(looks_binary(b"\x89HDF\r\n\x1a\n\x00\x00\x00"));
        assert!(looks_binary(&[b'a', b'b', 0u8, b'c']));
        // NUL beyond the 8 KiB window isn't scanned (treated as text).
        let mut late = vec![b'x'; 9000];
        late.push(0);
        assert!(!looks_binary(&late));
    }

    #[test]
    fn strip_truncate_ellipsizes_long_labels() {
        assert_eq!(strip_truncate("short"), "short");
        let long = "a".repeat(40);
        let t = strip_truncate(&long);
        assert_eq!(t.chars().count(), STRIP_MAX_LABEL);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn strip_target_centers_active() {
        let labels = vec!["aa".to_string(), "bbbb".to_string(), "cc".to_string()];
        let cell_w = 10.0;
        let gap = STRIP_GAP_CELLS * cell_w;
        // item0: w=20 center=10; gap=30 → cursor=50
        // item1: w=40 center=70
        assert!((session_strip_target(&labels, 0, cell_w) - 10.0).abs() < 1e-3);
        assert!((session_strip_target(&labels, 1, cell_w) - (50.0 + 20.0)).abs() < 1e-3);
        let _ = gap;
    }

    #[test]
    fn strip_lines_active_is_bold_and_centered() {
        let labels = vec!["aa".to_string(), "bbbb".to_string(), "cc".to_string()];
        let cell_w = 10.0;
        let win_w = 800.0;
        let active = 1;
        // Scroll exactly at the active center → active is screen-centered.
        let scroll = session_strip_target(&labels, active, cell_w);
        let lines = session_strip_lines(
            &labels,
            active,
            scroll,
            win_w,
            cell_w,
            500.0,
            &[],
            false,
            &[],
            &[],
        );
        // All three on-screen here.
        assert_eq!(lines.len(), 3);
        let act = &lines[1];
        assert!(act.bold && !act.dim, "active must be bold, not dim");
        // Active center == win_w/2: left + w/2 == 400.
        let w = 4.0 * cell_w;
        assert!(((act.x + w / 2.0) - win_w / 2.0).abs() < 1e-3);
        // Neighbours dim, not bold.
        assert!(lines[0].dim && !lines[0].bold);
        assert!(lines[2].dim && !lines[2].bold);
        // Ordering left→right: item0 left of item1 left of item2.
        assert!(lines[0].x < lines[1].x && lines[1].x < lines[2].x);
    }

    #[test]
    fn strip_lines_culls_offscreen() {
        // Many items, narrow window → far items culled.
        let labels: Vec<String> = (0..50).map(|i| format!("ws{i}")).collect();
        let cell_w = 10.0;
        let win_w = 300.0;
        let active = 25;
        let scroll = session_strip_target(&labels, active, cell_w);
        let lines = session_strip_lines(
            &labels,
            active,
            scroll,
            win_w,
            cell_w,
            500.0,
            &[],
            false,
            &[],
            &[],
        );
        assert!(
            lines.len() < labels.len(),
            "off-screen items must be culled"
        );
        // The active one is always present and centered.
        assert!(lines.iter().any(|l| l.bold));
    }

    #[test]
    fn strip_lines_color_by_work_state() {
        // Three sessions, item0 active+idle, item1 working, item2 blocked.
        let labels = vec!["aa".to_string(), "bb".to_string(), "cc".to_string()];
        let cell_w = 10.0;
        let win_w = 800.0;
        let active = 0;
        let scroll = session_strip_target(&labels, active, cell_w);
        let tones = vec![
            Some((AgentTone::Idle, false)),
            Some((AgentTone::Working, false)),
            Some((AgentTone::Blocked, false)),
        ];
        let lines = session_strip_lines(
            &labels,
            active,
            scroll,
            win_w,
            cell_w,
            500.0,
            &tones,
            false,
            &[],
            &[],
        );
        assert_eq!(lines.len(), 3);
        // Active idle keeps the bright default (idle = current colour), bold.
        assert_eq!(lines[0].color, Some((250, 250, 215)));
        assert!(lines[0].bold && !lines[0].dim);
        // Non-active working is green and NOT dim — visible off the active slot.
        assert_eq!(lines[1].color, AgentTone::Working.rgb());
        assert!(!lines[1].dim && !lines[1].bold);
        // Non-active blocked is red — the "needs you" signal pops.
        assert_eq!(lines[2].color, AgentTone::Blocked.rgb());
        assert!(!lines[2].dim);
    }

    #[test]
    fn strip_active_coloured_row_is_distinct_without_bold() {
        // Maintainer report: two COLOURED (working) sessions, one active — they
        // collapsed to identical-looking rows because active-vs-non-active
        // differed ONLY by bold, and the monospace face renders no real bold.
        // The contrast lever fixes it by giving the active coloured row a
        // BRIGHTNESS cue independent of Weight::BOLD. Earlier strip tests only
        // covered active-IDLE (which stands out via the bright default path),
        // never active-COLOURED — this is that missing case.
        let labels = vec!["aa".to_string(), "bb".to_string()];
        let cell_w = 10.0;
        let win_w = 800.0;
        let scroll = session_strip_target(&labels, 0, cell_w);
        let tones = vec![
            Some((AgentTone::Working, false)),
            Some((AgentTone::Working, false)),
        ];
        let base = AgentTone::Working.rgb();
        // Bright lever: active (item0) scaled up, non-active (item1) at base.
        let bright = session_strip_lines(
            &labels,
            0,
            scroll,
            win_w,
            cell_w,
            500.0,
            &tones,
            false,
            &[],
            &[],
        );
        assert_eq!(
            bright[0].color,
            base.map(|c| scale_rgb(c, CONTRAST_BRIGHT_FACTOR))
        );
        assert_eq!(bright[1].color, base);
        assert_ne!(
            bright[0].color, bright[1].color,
            "active coloured row must be distinct from a non-active same-tone row"
        );
        // Dim lever: active (item0) at base, non-active (item1) scaled down.
        let dim = session_strip_lines(
            &labels,
            0,
            scroll,
            win_w,
            cell_w,
            500.0,
            &tones,
            true,
            &[],
            &[],
        );
        assert_eq!(dim[0].color, base);
        assert_eq!(
            dim[1].color,
            base.map(|c| scale_rgb(c, CONTRAST_DIM_FACTOR))
        );
        assert_ne!(dim[0].color, dim[1].color);
    }

    #[test]
    fn strip_working_wilts_when_stale() {
        // A stale "working" still colours green but dims (the strip wilt).
        let labels = vec!["aa".to_string()];
        let cell_w = 10.0;
        let scroll = session_strip_target(&labels, 0, cell_w);
        let tones = vec![Some((AgentTone::Working, true))];
        let lines = session_strip_lines(
            &labels,
            9,
            scroll,
            800.0,
            cell_w,
            500.0,
            &tones,
            false,
            &[],
            &[],
        );
        assert_eq!(lines[0].color, AgentTone::Working.rgb());
        assert!(lines[0].dim, "a wilted working name dims on the strip too");
    }

    #[test]
    fn contrast_bright_pops_active_coloured_row() {
        // Bright lever (contrast_dim=false): the active coloured row is
        // scaled up past its base tone and bold; a non-active coloured row
        // keeps its full base tone, not bold.
        let base = AgentTone::Working.rgb().unwrap();
        let (act_rgb, act_bold, act_dim) =
            contrast_tone_rgb(AgentTone::Working, false, true, false, 0.0);
        assert!(act_bold && !act_dim);
        let act = act_rgb.unwrap();
        assert!(
            act.0 >= base.0 && act.1 >= base.1 && act.2 >= base.2 && act != base,
            "active bright row must be >= base tone and brighter overall"
        );
        // Non-active keeps base, no bold.
        let (na_rgb, na_bold, _) = contrast_tone_rgb(AgentTone::Working, false, false, false, 0.0);
        assert_eq!(na_rgb, Some(base));
        assert!(!na_bold);
    }

    #[test]
    fn contrast_dim_fades_non_active_coloured_row() {
        // Dim lever (contrast_dim=true): the active coloured row keeps its
        // base tone + bold; a non-active coloured row is scaled down.
        let base = AgentTone::Blocked.rgb().unwrap();
        let (act_rgb, act_bold, _) = contrast_tone_rgb(AgentTone::Blocked, false, true, true, 0.0);
        assert_eq!(act_rgb, Some(base));
        assert!(act_bold);
        let (na_rgb, na_bold, _) = contrast_tone_rgb(AgentTone::Blocked, false, false, true, 0.0);
        let na = na_rgb.unwrap();
        assert!(
            na.0 < base.0 && na.1 <= base.1 && na.2 <= base.2,
            "non-active dim row must be darker than base"
        );
        assert!(!na_bold);
    }

    #[test]
    fn contrast_dim_strip_fades_non_active_plain_name() {
        // Strip "dim" lever: a non-active idle/no-agent name is baked to an
        // explicitly-dimmed colour (not the default DIM flag).
        let labels = vec!["aa".to_string(), "bb".to_string()];
        let cell_w = 10.0;
        let scroll = session_strip_target(&labels, 0, cell_w);
        let tones: Vec<Option<(AgentTone, bool)>> = vec![None, None];
        let lines = session_strip_lines(
            &labels,
            0,
            scroll,
            800.0,
            cell_w,
            500.0,
            &tones,
            true,
            &[],
            &[],
        );
        // Active plain name stays bright + bold.
        assert_eq!(lines[0].color, Some((250, 250, 215)));
        assert!(lines[0].bold);
        // Non-active plain name carries an explicit dimmed colour, dim flag
        // off (the dim is baked into the colour).
        assert_eq!(
            lines[1].color,
            Some(scale_rgb((204, 204, 204), CONTRAST_DIM_FACTOR))
        );
        assert!(!lines[1].dim && !lines[1].bold);
    }

    #[test]
    fn flash_factor_ramps_down_over_window() {
        // Full brightness at the instant of the change.
        assert!((flash_factor(0.0) - 1.0).abs() < 1e-6);
        // Half-faded at half the window.
        assert!((flash_factor(FLASH_SECS / 2.0) - 0.5).abs() < 1e-6);
        // Zero at (and past) the window end — clamped, never negative.
        assert_eq!(flash_factor(FLASH_SECS), 0.0);
        assert_eq!(flash_factor(FLASH_SECS * 2.0), 0.0);
        assert_eq!(flash_factor(-1.0), 1.0);
    }

    #[test]
    fn flash_brightens_strip_name_toward_white() {
        // A full flash (f=1.0) lerps a working-green name all the way to
        // white; a finished flash (f=0.0) leaves the tone untouched. Use a
        // *non-active* name (active=1) so the contrast lever doesn't also
        // scale it — isolating the flash.
        let labels = vec!["aa".to_string(), "bb".to_string()];
        let cell_w = 10.0;
        let scroll = session_strip_target(&labels, 1, cell_w);
        let tones = vec![
            Some((AgentTone::Working, false)),
            Some((AgentTone::Idle, false)),
        ];
        let lit = session_strip_lines(
            &labels,
            1,
            scroll,
            800.0,
            cell_w,
            500.0,
            &tones,
            false,
            &[1.0, 0.0],
            &[],
        );
        assert_eq!(lit[0].color, Some((255, 255, 255)));
        let cold = session_strip_lines(
            &labels,
            1,
            scroll,
            800.0,
            cell_w,
            500.0,
            &tones,
            false,
            &[0.0, 0.0],
            &[],
        );
        assert_eq!(cold[0].color, AgentTone::Working.rgb());
    }

    #[test]
    fn pending_nav_status_names_workspace_and_path() {
        // The badge floor's user-facing status string (ADR 0025 §1) must name
        // both the workspace and the waiting path so the user knows where the
        // result is, and read as a switch prompt (non-disruptive — we never
        // yanked the view).
        let s = pending_nav_status("mypackage", "src/edge.jl");
        assert!(s.contains("mypackage"), "status names the workspace");
        assert!(s.contains("src/edge.jl"), "status names the pending path");
        assert!(
            s.contains("switch"),
            "status reads as a switch-to-view prompt, not a forced nav"
        );
    }

    #[test]
    fn pending_nav_insert_is_latest_wins() {
        // The pending_nav map is the badge-floor state mark_pending_nav writes:
        // keyed by workspace slug, latest path wins (a newer off-workspace
        // result supersedes the older one for the same workspace). This mirrors
        // mark_pending_nav's `insert` without needing a full State.
        let mut pending_nav: HashMap<String, String> = HashMap::new();
        pending_nav.insert("mypackage".to_string(), "src/a.jl".to_string());
        pending_nav.insert("other".to_string(), "src/b.jl".to_string());
        // Latest-wins on the same workspace.
        pending_nav.insert("mypackage".to_string(), "src/c.jl".to_string());
        assert_eq!(pending_nav.len(), 2, "one entry per workspace");
        assert_eq!(
            pending_nav.get("mypackage").map(String::as_str),
            Some("src/c.jl"),
            "latest result for a workspace supersedes the earlier one"
        );
        assert_eq!(
            pending_nav.get("other").map(String::as_str),
            Some("src/b.jl")
        );
    }

    #[test]
    fn ancestor_rels_is_deepest_first() {
        // Deep-path reveal expands the deepest *visible* ancestor each round-
        // trip, so the ordering must be deepest-first to walk down toward the
        // file one level at a time.
        assert_eq!(ancestor_rels("a/b/c.jl"), vec!["a/b", "a"]);
        // Root-level file: no ancestor dirs to expand (already a child of the
        // expanded root) → empty, so drive_reveal_step lands directly.
        assert!(ancestor_rels("README.md").is_empty());
        // Single dir.
        assert_eq!(ancestor_rels("src/edge.jl"), vec!["src"]);
        // Trailing slash (dir target) still yields its parents, deepest-first.
        assert_eq!(ancestor_rels("a/b/"), vec!["a/b", "a"]);
    }

    // ─── ADR 0025: FE_COMMAND routing (target filter + cmd→FeCommand) ─────

    fn fe_evt(
        cmd: &str,
        args: serde_json::Value,
        target: Option<&str>,
    ) -> sot_protocol::ops::FeCommandEvt {
        sot_protocol::ops::FeCommandEvt {
            v: 1,
            cmd: cmd.to_string(),
            args,
            target: target.map(|s| s.to_string()),
        }
    }

    #[test]
    fn route_target_none_acts_for_all_fes() {
        // target None = the badge floor: every FE acts.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            None,
        );
        let cmd = route_fe_command(&evt, "win-fe-host");
        assert!(matches!(cmd, Some(FeCommand::Preview { .. })));
    }

    #[test]
    fn route_target_self_acts() {
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            Some("win-fe-host"),
        );
        let cmd = route_fe_command(&evt, "win-fe-host");
        assert!(matches!(cmd, Some(FeCommand::Preview { .. })));
    }

    #[test]
    fn route_target_other_is_ignored() {
        // Scoped to a different FE — we ignore it (it's force-show for someone
        // else).
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            Some("win-fe-other"),
        );
        assert!(route_fe_command(&evt, "win-fe-host").is_none());
    }

    #[test]
    fn route_preview_urgent_is_directed_only() {
        // DIRECTED (target == self) + urgent → urgent honoured (force-show
        // eligible; the idle gate still applies later in dispatch).
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl", "urgent": true}),
            Some("win-fe-host"),
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Preview {
                workspace,
                path,
                urgent,
            }) => {
                assert_eq!(workspace, "ws");
                assert_eq!(path, "src/a.jl");
                assert!(
                    urgent,
                    "directed urgent is carried through (force-show eligible)"
                );
            }
            other => panic!("expected Preview, got {other:?}"),
        }
        // BROADCAST (target None = the badge floor) + urgent → urgent STRIPPED
        // to false. A broadcast must never force-show, or it would yank every
        // idle FE's view at once.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl", "urgent": true}),
            None,
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Preview { urgent, .. }) => assert!(
                !urgent,
                "broadcast urgent is stripped — badge floor only, never force-show"
            ),
            other => panic!("expected Preview, got {other:?}"),
        }
        // urgent absent → false regardless of target.
        let evt = fe_evt(
            "preview",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl"}),
            Some("win-fe-host"),
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Preview { urgent, .. }) => assert!(!urgent),
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    #[test]
    fn route_reveal_maps_to_reveal_with_urgent() {
        // Directed so urgent is honoured (force-show is directed-only).
        let evt = fe_evt(
            "reveal",
            serde_json::json!({"workspace": "ws", "path": "src/a.jl", "urgent": true}),
            Some("win-fe-host"),
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Reveal {
                workspace,
                path,
                urgent,
            }) => {
                assert_eq!(workspace, "ws");
                assert_eq!(path, "src/a.jl");
                assert!(urgent);
            }
            other => panic!("expected Reveal, got {other:?}"),
        }
    }

    #[test]
    fn route_goto_workspace_maps_to_workspace() {
        let evt = fe_evt(
            "goto_workspace",
            serde_json::json!({"workspace": "demo"}),
            None,
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Workspace { slug, boot }) => {
                assert_eq!(slug.as_deref(), Some("demo"));
                assert!(!boot, "boot defaults false when arg absent");
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn preview_same_ws_decision() {
        // Explicit slug == the active workspace -> render in place.
        assert!(preview_targets_active_ws(
            Some("myanalysis"),
            Some("ship_of_tools"),
            "myanalysis"
        ));
        // Explicit slug != active -> NOT same-ws (force-show / badge path).
        assert!(!preview_targets_active_ws(
            Some("myanalysis"),
            Some("ship_of_tools"),
            "ship_of_tools"
        ));
        // On the default workspace (active id None): targeting it by slug,
        // or by "default"/"<default>"/"" all resolve to same-ws.
        assert!(preview_targets_active_ws(
            None,
            Some("ship_of_tools"),
            "ship_of_tools"
        ));
        assert!(preview_targets_active_ws(
            None,
            Some("ship_of_tools"),
            "default"
        ));
        assert!(preview_targets_active_ws(
            None,
            Some("ship_of_tools"),
            "<default>"
        ));
        assert!(preview_targets_active_ws(None, Some("ship_of_tools"), ""));
        // On a NON-default ws, targeting "default" is a real cross-ws switch,
        // not same-ws (so it must NOT short-circuit to in-place render).
        assert!(!preview_targets_active_ws(
            Some("myanalysis"),
            Some("ship_of_tools"),
            "default"
        ));
        // No default slug known yet (pre-hello) + "default" target -> can't
        // resolve, so not same-ws (falls through to the safe badge path).
        assert!(!preview_targets_active_ws(None, None, "default"));
    }

    #[test]
    fn route_goto_workspace_boot_flag() {
        // `sot-fe goto --boot <ws>` → args carry boot:true → FeCommand
        // seeds autostart before the switch (scriptable spawn->goto->boot).
        let evt = fe_evt(
            "goto_workspace",
            serde_json::json!({"workspace": "demo", "boot": true}),
            Some("win-fe-host"),
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Workspace { slug, boot }) => {
                assert_eq!(slug.as_deref(), Some("demo"));
                assert!(boot, "boot:true must thread through");
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn route_goto_mode_maps_to_mode() {
        let evt = fe_evt("goto_mode", serde_json::json!({"mode": "modules"}), None);
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Mode { mode }) => assert_eq!(mode, "modules"),
            other => panic!("expected Mode, got {other:?}"),
        }
    }

    #[test]
    fn route_notify_maps_with_optional_level() {
        let evt = fe_evt(
            "notify",
            serde_json::json!({"text": "build done", "level": "info"}),
            None,
        );
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Notify { text, level }) => {
                assert_eq!(text, "build done");
                assert_eq!(level.as_deref(), Some("info"));
            }
            other => panic!("expected Notify, got {other:?}"),
        }
        // level is optional.
        let evt = fe_evt("notify", serde_json::json!({"text": "hi"}), None);
        match route_fe_command(&evt, "win-fe-host") {
            Some(FeCommand::Notify { level, .. }) => assert!(level.is_none()),
            other => panic!("expected Notify, got {other:?}"),
        }
    }

    #[test]
    fn route_unknown_cmd_is_ignored() {
        let evt = fe_evt("explode", serde_json::json!({}), None);
        assert!(route_fe_command(&evt, "win-fe-host").is_none());
    }

    #[test]
    fn route_missing_required_arg_is_ignored() {
        // preview without path.
        let evt = fe_evt("preview", serde_json::json!({"workspace": "ws"}), None);
        assert!(route_fe_command(&evt, "win-fe-host").is_none());
        // preview without workspace.
        let evt = fe_evt("preview", serde_json::json!({"path": "src/a.jl"}), None);
        assert!(route_fe_command(&evt, "win-fe-host").is_none());
        // goto_workspace without workspace.
        let evt = fe_evt("goto_workspace", serde_json::json!({}), None);
        assert!(route_fe_command(&evt, "win-fe-host").is_none());
        // notify without text.
        let evt = fe_evt("notify", serde_json::json!({"level": "info"}), None);
        assert!(route_fe_command(&evt, "win-fe-host").is_none());
    }

    #[test]
    fn self_comm_handle_is_win_fe_lowercased_host() {
        // Deterministic shape: win-fe-<lowercased host>. We don't assert the
        // exact host (env-dependent) but the prefix + lowercasing invariant.
        let h = self_comm_handle();
        assert!(h.starts_with("win-fe-"), "got {h:?}");
        assert_eq!(h, h.to_lowercase(), "handle is lowercased");
    }

    #[test]
    fn strip_pending_badges_name_with_sigil_and_accent() {
        // Badge floor (ADR 0025 §1): a workspace flagged pending gets a leading
        // `●` sigil and bright white + bold on the bottom strip, overriding
        // whatever work-state colour it would otherwise carry — so it reads as
        // "result waiting here", distinct from working/idle/blocked. The view is
        // untouched; only the rendering changes.
        let labels = vec!["aa".to_string(), "bb".to_string()];
        let cell_w = 10.0;
        let win_w = 800.0;
        let scroll = session_strip_target(&labels, 0, cell_w);
        // bb is "working" (green) but ALSO pending — pending must win.
        let tones = vec![None, Some((AgentTone::Working, false))];
        let pendings = vec![false, true];
        let lines = session_strip_lines(
            &labels,
            0,
            scroll,
            win_w,
            cell_w,
            500.0,
            &tones,
            false,
            &[],
            &pendings,
        );
        assert_eq!(lines.len(), 2);
        // Non-pending name keeps its plain text, no sigil.
        assert!(!lines[0].text.starts_with('●'));
        // Pending name carries the sigil + bright white, overriding the
        // green working tone, and is bold so it pops.
        assert!(
            lines[1].text.starts_with('●'),
            "pending name gets the ● sigil"
        );
        assert_eq!(
            lines[1].color,
            Some((255, 255, 255)),
            "pending name uses bright white, not the working green"
        );
        assert!(lines[1].bold, "pending name is bold so it stands out");
        assert!(!lines[1].dim);
    }

    #[test]
    fn parent_files_node_id_strips_last_segment() {
        assert_eq!(parent_files_node_id("files:foo/bar.txt"), "files:foo");
        assert_eq!(parent_files_node_id("files:a/b/c"), "files:a/b");
        // Root-level file → the root node.
        assert_eq!(parent_files_node_id("files:bar.txt"), "files:");
        // Non-files ids pass through.
        assert_eq!(parent_files_node_id("modules:Foo"), "modules:Foo");
    }

    #[test]
    fn nav_envelope_parses_valid_v1() {
        let txt = r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"mypackage","mode":"files","path":"src/edge.jl"}}"#;
        assert_eq!(
            parse_nav_envelope(txt),
            Some(NavEnvelope {
                workspace: "mypackage".to_string(),
                path: "src/edge.jl".to_string(),
            })
        );
    }

    #[test]
    fn nav_envelope_tolerates_surrounding_whitespace() {
        let txt = "  \n{\"sot_ui\":{\"v\":1,\"cmd\":\"nav.preview\",\"workspace\":\"w\",\"path\":\"a/b.md\"}}\n ";
        assert_eq!(
            parse_nav_envelope(txt),
            Some(NavEnvelope {
                workspace: "w".to_string(),
                path: "a/b.md".to_string()
            })
        );
    }

    #[test]
    fn nav_envelope_rejects_ordinary_chat() {
        // Ordinary prose, empty, malformed JSON, and valid-but-foreign JSON
        // all fall through to normal inbox rendering.
        assert_eq!(parse_nav_envelope("hey, look at src/edge.jl?"), None);
        assert_eq!(parse_nav_envelope(""), None);
        assert_eq!(parse_nav_envelope("{not json"), None);
        assert_eq!(parse_nav_envelope(r#"{"hello":"world"}"#), None);
    }

    #[test]
    fn nav_envelope_rejects_wrong_version_cmd_or_missing_fields() {
        // Wrong version.
        assert_eq!(
            parse_nav_envelope(
                r#"{"sot_ui":{"v":2,"cmd":"nav.preview","workspace":"w","path":"p"}}"#
            ),
            None
        );
        // Unknown cmd (v1 only handles nav.preview).
        assert_eq!(
            parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.jump","workspace":"w","path":"p"}}"#),
            None
        );
        // Missing path / missing workspace.
        assert_eq!(
            parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"w"}}"#),
            None
        );
        assert_eq!(
            parse_nav_envelope(r#"{"sot_ui":{"v":1,"cmd":"nav.preview","path":"p"}}"#),
            None
        );
        // Empty workspace / path.
        assert_eq!(
            parse_nav_envelope(
                r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"","path":"p"}}"#
            ),
            None
        );
        assert_eq!(
            parse_nav_envelope(
                r#"{"sot_ui":{"v":1,"cmd":"nav.preview","workspace":"w","path":""}}"#
            ),
            None
        );
    }

    #[test]
    fn build_new_file_node_id_joins_and_validates() {
        // Root dir (`files:`) → no separator before the bare name.
        assert_eq!(
            build_new_file_node_id("files:", "a.txt"),
            Ok("files:a.txt".to_string())
        );
        // Sub-directory → joined with a single `/`.
        assert_eq!(
            build_new_file_node_id("files:sub", "a.txt"),
            Ok("files:sub/a.txt".to_string())
        );
        assert_eq!(
            build_new_file_node_id("files:a/b", "c.jl"),
            Ok("files:a/b/c.jl".to_string())
        );
        // Surrounding whitespace is trimmed off the name.
        assert_eq!(
            build_new_file_node_id("files:sub", "  spaced.txt  "),
            Ok("files:sub/spaced.txt".to_string())
        );
        // Empty / whitespace-only names are rejected.
        assert!(build_new_file_node_id("files:", "").is_err());
        assert!(build_new_file_node_id("files:", "   ").is_err());
        // Path separators are rejected (the backend only accepts a bare
        // child segment — no nested-dir creation, no `..` traversal).
        assert!(build_new_file_node_id("files:", "a/b.txt").is_err());
        assert!(build_new_file_node_id("files:", "a\\b.txt").is_err());
        assert!(build_new_file_node_id("files:sub", "../escape.txt").is_err());
    }

    #[test]
    fn new_file_collision_detected_against_existing_sibling() {
        // Mirror the confirm-path collision guard: scan the flat rows for the
        // would-be new id. A sibling `a.txt` already under `files:sub` means
        // creating another `a.txt` there collides; `b.txt` does not.
        let mut t = TreeView::new();
        t.set_root(
            node("files:sub", "sub", true),
            vec![node("files:sub/a.txt", "a.txt", false)],
        );
        let collide = build_new_file_node_id("files:sub", "a.txt").unwrap();
        let fresh = build_new_file_node_id("files:sub", "b.txt").unwrap();
        assert!(t.rows.iter().any(|r| r.node.id == collide));
        assert!(!t.rows.iter().any(|r| r.node.id == fresh));
    }

    #[test]
    fn delete_refuses_directory_rows() {
        // Mirror the Ctrl+N tests: `begin_delete_file` pre-refuses dirs in v1,
        // so its dir test (`is_directory_row`) must reject a `dir`-kind node
        // and the files root, while accepting an ordinary file row. A directory
        // row (kind "dir").
        let dir = TreeNode {
            id: "files:sub".to_string(),
            label: "sub".to_string(),
            kind: "dir".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        assert!(is_directory_row(&dir), "kind == dir is a directory");
        // The files root is itself a directory even without the "dir" kind.
        let root = TreeNode {
            id: "files:".to_string(),
            label: "/".to_string(),
            kind: "files".to_string(),
            has_children: true,
            badges: Vec::new(),
            payload: Default::default(),
        };
        assert!(is_directory_row(&root), "files: root is a directory");
        // An ordinary file row is deletable (not a directory). `node` builds a
        // file row (kind "files", no children).
        let file = node("files:sub/a.txt", "a.txt", false);
        assert!(!is_directory_row(&file), "a file row is deletable");
    }

    #[test]
    fn set_root_seeds_children_at_depth_one() {
        let mut t = TreeView::new();
        t.set_root(
            node("files:", "root", true),
            vec![node("files:a", "a", true), node("files:b", "b", false)],
        );
        assert_eq!(t.rows.len(), 3);
        assert_eq!(t.rows[0].depth, 0);
        assert!(t.rows[0].expanded);
        assert_eq!(t.rows[1].depth, 1);
        assert_eq!(t.rows[2].depth, 1);
        assert_eq!(t.selected, 0);
    }

    #[test]
    fn apply_children_splices_under_parent_and_replaces_existing() {
        let mut t = TreeView::new();
        t.set_root(
            node("r", "r", true),
            vec![node("a", "a", true), node("b", "b", false)],
        );
        t.apply_children(
            "a",
            vec![node("a/1", "a1", false), node("a/2", "a2", false)],
        );
        // rows: r, a, a1, a2, b
        assert_eq!(
            t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
            vec!["r", "a", "a/1", "a/2", "b"]
        );
        assert!(t.rows[1].expanded);
        assert_eq!(t.rows[2].depth, 2);
        // re-applying with a different set replaces the previous children
        t.apply_children("a", vec![node("a/x", "ax", false)]);
        assert_eq!(
            t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
            vec!["r", "a", "a/x", "b"]
        );
    }

    #[test]
    fn collapse_selected_drops_descendants_and_unflags() {
        let mut t = TreeView::new();
        t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
        t.apply_children("a", vec![node("a/1", "a1", false)]);
        t.selected = 1; // on `a`
        assert!(t.collapse_selected());
        assert_eq!(
            t.rows.iter().map(|r| r.node.id.clone()).collect::<Vec<_>>(),
            vec!["r", "a"]
        );
        assert!(!t.rows[1].expanded);
        // collapsing a leaf is a no-op
        t.selected = 1;
        assert!(!t.collapse_selected());
    }

    #[test]
    fn parent_of_selected_walks_back_to_lesser_depth() {
        let mut t = TreeView::new();
        t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
        t.apply_children("a", vec![node("a/1", "a1", false)]);
        t.selected = 2; // on a/1, depth=2
        assert_eq!(t.parent_of_selected(), Some(1));
        t.selected = 0;
        assert_eq!(t.parent_of_selected(), None);
    }

    #[test]
    fn move_up_and_down_clamp_at_edges() {
        let mut t = TreeView::new();
        t.set_root(
            node("r", "r", true),
            vec![node("a", "a", false), node("b", "b", false)],
        );
        t.move_up(); // already at 0
        assert_eq!(t.selected, 0);
        t.move_down();
        t.move_down();
        t.move_down(); // last row is 2; should clamp
        assert_eq!(t.selected, 2);
    }

    // ---- cursor-by-node-id preservation across row-mutating ops ----
    //
    // Regression coverage for the "nav cursor resets to row 0" bug. set_root,
    // set_flat, and apply_children all used to unconditionally reset
    // `selected` to 0, which kicked the cursor back to the top of the nav
    // every transport reconnect, every project.scan, and every stale
    // tree.children splice. The fix re-anchors on the previously-cursored
    // node id; these tests pin that behaviour.

    #[test]
    fn set_root_preserves_cursor_when_node_still_present() {
        let mut t = TreeView::new();
        t.set_root(
            node("r", "r", true),
            vec![node("a", "a", false), node("b", "b", false)],
        );
        t.selected = 2; // on `b`
        t.set_root(
            node("r", "r", true),
            vec![
                node("a", "a", false),
                node("b", "b", false),
                node("c", "c", false),
            ],
        );
        assert_eq!(t.rows[t.selected].node.id, "b");
    }

    #[test]
    fn set_root_falls_back_to_zero_when_cursored_node_gone() {
        let mut t = TreeView::new();
        t.set_root(
            node("r", "r", true),
            vec![node("a", "a", false), node("b", "b", false)],
        );
        t.selected = 2; // on `b`
        t.set_root(
            node("r2", "r2", true),
            vec![node("x", "x", false), node("y", "y", false)],
        );
        assert_eq!(t.selected, 0);
        assert_eq!(t.rows[0].node.id, "r2");
    }

    #[test]
    fn set_flat_preserves_cursor_when_node_still_present() {
        let mut t = TreeView::new();
        let rows = |ids: &[&str]| -> Vec<TreeRow> {
            ids.iter()
                .map(|id| TreeRow {
                    node: node(id, id, false),
                    depth: 0,
                    expanded: false,
                })
                .collect()
        };
        t.set_flat(rows(&["m1", "m2", "m3"]));
        t.selected = 2; // on `m3`
        t.set_flat(rows(&["m1", "m2", "m3", "m4"]));
        assert_eq!(t.rows[t.selected].node.id, "m3");
    }

    #[test]
    fn set_flat_falls_back_to_zero_when_cursored_node_gone() {
        let mut t = TreeView::new();
        let rows = |ids: &[&str]| -> Vec<TreeRow> {
            ids.iter()
                .map(|id| TreeRow {
                    node: node(id, id, false),
                    depth: 0,
                    expanded: false,
                })
                .collect()
        };
        t.set_flat(rows(&["m1", "m2", "m3"]));
        t.selected = 2;
        t.set_flat(rows(&["n1", "n2"]));
        assert_eq!(t.selected, 0);
    }

    #[test]
    fn apply_children_preserves_cursor_below_splice() {
        let mut t = TreeView::new();
        t.set_root(
            node("r", "r", true),
            vec![node("a", "a", true), node("b", "b", false)],
        );
        t.selected = 2; // on `b`, after the splice point under `a`
        t.apply_children(
            "a",
            vec![node("a/1", "a1", false), node("a/2", "a2", false)],
        );
        // rows: r, a, a/1, a/2, b — cursor follows `b` to its new index
        assert_eq!(t.rows[t.selected].node.id, "b");
        assert_eq!(t.selected, 4);
    }

    #[test]
    fn apply_children_falls_back_to_parent_when_cursored_child_disappears() {
        let mut t = TreeView::new();
        t.set_root(node("r", "r", true), vec![node("a", "a", true)]);
        t.apply_children(
            "a",
            vec![node("a/1", "a1", false), node("a/2", "a2", false)],
        );
        t.selected = 2; // on a/1
                        // Re-apply with a different set; a/1 is gone.
        t.apply_children("a", vec![node("a/x", "ax", false)]);
        // rows: r, a, a/x — cursor falls back to the parent (`a`) so the
        // user stays anchored to the surrounding context.
        assert_eq!(t.rows[t.selected].node.id, "a");
        assert_eq!(t.selected, 1);
    }

    #[test]
    #[test]
    fn split_frontmatter_returns_header_and_body() {
        let s = "---\ntarget: x\nsynced_against: abc\n---\n# Body\n\nText.\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(
            h.as_deref(),
            Some("---\ntarget: x\nsynced_against: abc\n---\n")
        );
        assert_eq!(b, "# Body\n\nText.\n");
        // Round-trip: header + body == original.
        assert_eq!(h.unwrap() + &b, s);
    }

    #[test]
    fn split_frontmatter_no_header_returns_none() {
        let s = "# Just markdown\n\nNo frontmatter.\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(h, None);
        assert_eq!(b, s);
    }

    #[test]
    fn split_frontmatter_unterminated_treated_as_no_header() {
        let s = "---\ntarget: x\n# never closed\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(h, None);
        assert_eq!(b, s);
    }

    #[test]
    fn split_frontmatter_empty_body_after_header() {
        let s = "---\ntarget: x\n---\n";
        let (h, b) = split_frontmatter(s);
        assert_eq!(h.as_deref(), Some("---\ntarget: x\n---\n"));
        assert_eq!(b, "");
        assert_eq!(h.unwrap() + &b, s);
    }

    #[test]
    fn strip_frontmatter_removes_yaml_block() {
        // Trailing newline is preserved now — the new `split_frontmatter`
        // back-end uses `s.split('\n')` so the round-trip (header +
        // body) reproduces the source byte-perfect, which the edit
        // flow needs to keep `concept.write` payloads stable.
        let s = "---\ntarget: foo\nsynced_against: hash\n---\n# Body\n\nText.\n";
        assert_eq!(strip_frontmatter(s), "# Body\n\nText.\n");
    }

    #[test]
    fn strip_frontmatter_passthrough_when_no_block() {
        let s = "# Title\n\nNo frontmatter here.";
        assert_eq!(strip_frontmatter(s), s);
    }

    #[test]
    fn strip_frontmatter_passthrough_when_unterminated() {
        let s = "---\ntarget: foo\n# But no closing fence\n\nBody.";
        assert_eq!(strip_frontmatter(s), s);
    }

    #[test]
    fn strip_frontmatter_handles_empty_body() {
        let s = "---\ntarget: foo\n---\n";
        // Lines after the closing fence: empty trailing line. join("\n") = "".
        assert_eq!(strip_frontmatter(s), "");
    }

    #[test]
    fn parse_synced_against_bare_value() {
        let s = "---\ntarget: foo\nsynced_against: abc123\n---\n# Body\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_synced_against_double_quoted() {
        let s = "---\nsynced_against: \"abc123\"\n---\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_synced_against_single_quoted() {
        let s = "---\nsynced_against: 'abc123'\n---\n";
        assert_eq!(parse_synced_against(s).as_deref(), Some("abc123"));
    }

    #[test]
    fn parse_synced_against_missing_field() {
        let s = "---\ntarget: foo\nauthored_by: x\n---\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn parse_synced_against_no_frontmatter() {
        let s = "# Just markdown, no frontmatter\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn parse_synced_against_empty_value() {
        let s = "---\nsynced_against:\n---\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn parse_synced_against_field_in_body_ignored() {
        // The closing `---` ends scanning before we see this line.
        let s = "---\ntarget: foo\n---\nsynced_against: not-real\n";
        assert_eq!(parse_synced_against(s), None);
    }

    #[test]
    fn node_id_to_target_files_and_modules() {
        assert_eq!(
            node_id_to_concept_target("files:rust/foo.rs").as_deref(),
            Some("files/rust/foo.rs")
        );
        assert_eq!(
            node_id_to_concept_target("modules:Foo").as_deref(),
            Some("modules/Foo")
        );
        assert_eq!(node_id_to_concept_target("files:"), None);
        assert_eq!(node_id_to_concept_target("modules:"), None);
        assert_eq!(node_id_to_concept_target("unknown:Foo"), None);
    }

    fn entry(eval_id: u64, code: &str, in_flight: bool) -> ReplEntry {
        ReplEntry {
            eval_id,
            code: code.to_string(),
            frames: Vec::new(),
            elapsed_ms: 0,
            in_flight,
            pkg_mode: false,
        }
    }

    #[test]
    fn history_step_back_returns_none_on_empty_log() {
        let log: Vec<ReplEntry> = Vec::new();
        let mut pos = None;
        let mut saved = None;
        assert_eq!(history_step_back(&log, &mut pos, &mut saved, "draft"), None);
        assert!(pos.is_none());
        assert!(saved.is_none());
    }

    #[test]
    fn history_step_back_skips_in_flight_entries() {
        let log = vec![entry(1, "x = 1", false), entry(2, "x = 2", true)];
        let mut pos = None;
        let mut saved = None;
        // Only the completed entry is a candidate, so back lands on it.
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "draft").as_deref(),
            Some("x = 1")
        );
        // Saved on entry to the walk.
        assert_eq!(saved.as_deref(), Some("draft"));
        // No older entry; second back is a no-op.
        assert_eq!(history_step_back(&log, &mut pos, &mut saved, "x = 1"), None);
    }

    #[test]
    fn history_step_back_walks_oldest_to_newest_in_reverse() {
        let log = vec![
            entry(1, "first", false),
            entry(2, "second", false),
            entry(3, "third", false),
        ];
        let mut pos = None;
        let mut saved = None;
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "").as_deref(),
            Some("third")
        );
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "third").as_deref(),
            Some("second")
        );
        assert_eq!(
            history_step_back(&log, &mut pos, &mut saved, "second").as_deref(),
            Some("first")
        );
        // At the oldest; further back returns None and pos stays put.
        assert_eq!(history_step_back(&log, &mut pos, &mut saved, "first"), None);
        assert_eq!(pos, Some(0));
    }

    #[test]
    fn history_step_forward_no_op_when_not_walking() {
        let log = vec![entry(1, "a", false)];
        let mut pos = None;
        let mut saved = None;
        assert_eq!(history_step_forward(&log, &mut pos, &mut saved), None);
    }

    #[test]
    fn history_step_forward_past_newest_restores_saved_buffer() {
        let log = vec![entry(1, "first", false), entry(2, "second", false)];
        let mut pos = None;
        let mut saved = None;
        // Walk back twice; saved captures the original draft.
        let _ = history_step_back(&log, &mut pos, &mut saved, "in-progress");
        let _ = history_step_back(&log, &mut pos, &mut saved, "second");
        assert_eq!(pos, Some(0));
        // Forward once: back to "second".
        assert_eq!(
            history_step_forward(&log, &mut pos, &mut saved).as_deref(),
            Some("second")
        );
        // Forward again: past newest, restore "in-progress", exit walk.
        assert_eq!(
            history_step_forward(&log, &mut pos, &mut saved).as_deref(),
            Some("in-progress")
        );
        assert!(pos.is_none());
        assert!(saved.is_none());
    }

    #[test]
    fn history_step_forward_with_no_saved_buffer_yields_empty_string() {
        // Shouldn't normally happen (saved is set on first back), but
        // verify the unwrap_or_default fallback doesn't panic.
        let log = vec![entry(1, "x", false)];
        let mut pos = Some(0);
        let mut saved: Option<String> = None;
        assert_eq!(
            history_step_forward(&log, &mut pos, &mut saved).as_deref(),
            Some("")
        );
        assert!(pos.is_none());
    }
}
