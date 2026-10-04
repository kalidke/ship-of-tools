// settings.rs — user-configurable layout (and future general) settings
// for the frontend.
//
// Sibling to ui/input/keybindings.rs and following the same layered-discovery
// pattern, so both files feel consistent and the user (or the LLM
// editing on their behalf) only has to learn one shape.
//
// File discovery, in priority order:
//   1. $SOT_SETTINGS  — explicit override path
//   2. the nearest .sot/settings.toml in the cwd or an ancestor  — project-level
//   3. $HOME/.config/sot/settings.toml  — user-level
//
// File format (aspect-ratio-keyed presets,
// no in-session reflow):
//
//   [layout]
//   preset = "auto"   # auto | ultrawide | laptop | portrait
//
//   [layout.ultrawide]              # primary monitor aspect > 1.9
//   columns       = "nav,preview,llm"
//   widths        = "0.167,0.333,0.5"
//   drawer        = "repl"
//   drawer_height = "0.35"
//
//   [layout.laptop]                 # 1.5 ≤ aspect ≤ 1.9
//   columns       = "nav,preview,llm"
//   widths        = "0.18,0.32,0.50"
//   drawer        = "repl"
//   drawer_height = "0.40"
//
//   [layout.portrait]               # aspect < 1.5
//   columns       = "nav,preview"
//   widths        = "0.30,0.70"
//   drawer        = "repl"
//   drawer_height = "0.40"
//
//   [repl]
//   auto_open_drawer_on_run = true  # `r`/`R` from NavTree auto-open the
//                                   # REPL drawer (keeps NavTree focus)
//
//   [terminal]
//   shell = "/usr/bin/fish"         # Override the auto-resolved shell.
//                                   # Auto: $SHELL → /bin/bash → /bin/sh
//                                   # (Unix) or pwsh.exe → powershell.exe
//                                   # → cmd.exe (Windows). Omit to use
//                                   # the platform default.
//
//   [downloads]
//   dir = "/home/me/sot-downloads" # Local directory that `d` (download)
//                                   # writes files into. Omit to use the OS
//                                   # download dir (Win %USERPROFILE%\Downloads,
//                                   # Linux XDG_DOWNLOAD_DIR/~/Downloads, mac
//                                   # ~/Downloads), falling back to cwd.
//
//   [nav]
//   spill_ms = 2000                 # While the nav cursor is moving, rows
//                                   # too long for the nav column float
//                                   # their full text over the preview
//                                   # pane's left edge (the panes don't
//                                   # move), vanishing this many ms after
//                                   # the last move. 0 disables.
//
//   [gpu]
//   power_preference = "low"        # low (default, integrated) | high
//                                   # (discrete). "low" keeps the dGPU
//                                   # asleep on hybrid-graphics laptops.
//                                   # Binds at surface creation — needs a
//                                   # frontend restart to take effect.
//
//   [display]
//   fullscreen_vsync_pin = false    # default false. While fullscreen, keep
//                                   # a steady vsync-paced redraw instead of
//                                   # the efficient on-demand idle path.
//                                   # Set true on a VRR/adaptive-sync OLED
//                                   # panel that pumps brightness in
//                                   # borderless fullscreen (see ui/app/handler.rs);
//                                   # everyone else stays off. Costs 25.5
//                                   # points of one core + 8.5 points of
//                                   # iGPU 3D continuously at idle in
//                                   # fullscreen (measured on one laptop).
//
// `columns` lists named slots in left-to-right order; `widths` is the
// matching fractional split (must sum to ~1.0; values renormalised on
// parse). Valid slot names: nav, preview, llm. The drawer is a
// separate bottom strip; `drawer` names the slot rendered there
// (today only "repl") and `drawer_height` is its fraction of window
// height when open.
//
// Out-of-range or unparseable values warn and fall back to the
// default; the chrome should never crash because the user wrote a
// malformed settings file. We stick with the hand-rolled parser to
// keep the dep graph minimal — supports `[section]`, `[a.b]`,
// `key = value`, and comma-list strings for arrays.

use std::fs;
use std::path::PathBuf;

use super::discover::find_config_file;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetMode {
    /// Pick by primary-monitor aspect ratio at startup. Default.
    Auto,
    Ultrawide,
    Laptop,
    Portrait,
}

impl PresetMode {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(PresetMode::Auto),
            "ultrawide" => Some(PresetMode::Ultrawide),
            "laptop" => Some(PresetMode::Laptop),
            "portrait" => Some(PresetMode::Portrait),
            _ => None,
        }
    }
}

/// Which GPU the frontend asks wgpu for at adapter selection.
///
/// We render glyph quads and image blits — a 2D workload an integrated
/// GPU handles comfortably — so `Low` is the default. On hybrid-graphics
/// laptops (Optimus and friends) asking for the discrete GPU keeps it
/// awake for the whole session: measured ~11 W / 54 C on an idle RTX 4070
/// drawing a text UI (2026-07-31). The cost is the dGPU's inability to
/// power-gate while it owns an active surface, not the Optimus frame copy
/// (which is cheap at our sizes), so the fix is "don't wake it".
///
/// On single-adapter machines this is a no-op: with only an iGPU present,
/// `High` already resolves to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuPowerPreference {
    /// Prefer the integrated / low-power adapter. Default.
    Low,
    /// Prefer the discrete / high-performance adapter. For desktops with a
    /// real GPU, or if a low-power adapter renders incorrectly.
    High,
}

impl GpuPowerPreference {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            // The wgpu spellings are accepted too — the user reading
            // ui/render/surface.rs shouldn't have to translate.
            "low" | "low_power" | "lowpower" | "integrated" => Some(GpuPowerPreference::Low),
            "high" | "high_performance" | "highperformance" | "discrete" => {
                Some(GpuPowerPreference::High)
            }
            _ => None,
        }
    }
}

/// One named slot in a column row. Only these four are recognised
/// today; unknown names parse to `None` and the column is skipped
/// with a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Nav,
    Preview,
    Llm,
    Repl,
}

impl Slot {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "nav" => Some(Slot::Nav),
            "preview" => Some(Slot::Preview),
            "llm" => Some(Slot::Llm),
            "repl" => Some(Slot::Repl),
            _ => None,
        }
    }

    #[allow(dead_code)] // used by future status-line / diagnostics; kept now so the API is complete
    pub fn label(self) -> &'static str {
        match self {
            Slot::Nav => "nav",
            Slot::Preview => "preview",
            Slot::Llm => "llm",
            Slot::Repl => "repl",
        }
    }
}

/// One aspect-bucket's layout: columns left-to-right + their widths +
/// optional bottom drawer.
#[derive(Debug, Clone)]
pub struct LayoutPreset {
    pub columns: Vec<Slot>,
    /// Fractional column widths summing to (approximately) 1.0.
    /// Same length as `columns`. Renormalised at parse time so the
    /// user can write `0.167,0.333,0.5` and not worry about a stray
    /// 0.001.
    pub widths: Vec<f32>,
    /// Slot rendered as the bottom drawer when toggled open (e.g.
    /// `Slot::Repl`). `None` = no drawer in this preset.
    pub drawer: Option<Slot>,
    /// Drawer's fractional height when open. Ignored when `drawer` is
    /// `None`. Clamped to `[DRAWER_MIN, DRAWER_MAX]`.
    pub drawer_height: f32,
}

const DRAWER_MIN: f32 = 0.10;
const DRAWER_MAX: f32 = 0.80;

impl LayoutPreset {
    /// Built-in default for ultrawide aspects (>1.9). Matches the
    /// VS Code-style 1/6 · 1/3 · 1/2 nav | preview | llm split the
    /// user described, with REPL relegated to a hidden bottom drawer.
    pub fn default_ultrawide() -> Self {
        Self {
            columns: vec![Slot::Nav, Slot::Preview, Slot::Llm],
            widths: vec![0.167, 0.333, 0.500],
            drawer: Some(Slot::Repl),
            drawer_height: 0.35,
        }
    }

    /// Built-in default for laptop / 16:10 / 16:9 aspects
    /// (1.5 ≤ aspect ≤ 1.9). Narrower nav, more LLM than ultrawide
    /// since horizontal real estate is tighter.
    pub fn default_laptop() -> Self {
        Self {
            columns: vec![Slot::Nav, Slot::Preview, Slot::Llm],
            widths: vec![0.18, 0.32, 0.50],
            drawer: Some(Slot::Repl),
            drawer_height: 0.40,
        }
    }

    /// Built-in default for portrait / split-screen / very narrow
    /// aspects (<1.5). Drops the LLM column — at this width you flip
    /// between preview and LLM via a key rather than seeing both. For
    /// the v1 we just hide the LLM; a follow-up can wire the toggle.
    pub fn default_portrait() -> Self {
        Self {
            columns: vec![Slot::Nav, Slot::Preview],
            widths: vec![0.30, 0.70],
            drawer: Some(Slot::Repl),
            drawer_height: 0.40,
        }
    }

    /// Wide-preview variant (`layout.wide_preview` toggle): the Llm
    /// column is removed and its width handed to Preview, so nav keeps
    /// its width and the preview absorbs the rest of the screen. A
    /// preset with no Llm column (e.g. portrait) comes back unchanged;
    /// with no Preview column the freed width goes to the last column
    /// so the widths still sum to ~1.0.
    pub fn wide_preview(&self) -> Self {
        let mut p = self.clone();
        if let Some(idx) = p.columns.iter().position(|s| *s == Slot::Llm) {
            let w = p.widths.remove(idx);
            p.columns.remove(idx);
            if let Some(pi) = p.columns.iter().position(|s| *s == Slot::Preview) {
                p.widths[pi] += w;
            } else if let Some(last) = p.widths.last_mut() {
                *last += w;
            }
        }
        p
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    /// Which preset is active. `Auto` resolves to the named preset
    /// matching the primary monitor's aspect ratio at startup; the
    /// other three values lock to that preset regardless of aspect.
    pub preset: PresetMode,
    pub ultrawide: LayoutPreset,
    pub laptop: LayoutPreset,
    pub portrait: LayoutPreset,
    /// `[repl] auto_open_drawer_on_run` — when running a `.jl` file from
    /// NavTree via `r`/`R`, auto-open the (closed) REPL drawer so the run's
    /// output is visible. Keeps NavTree focus so `r`/`R` stay usable.
    /// Default `true`.
    pub repl_auto_open_drawer_on_run: bool,
    /// `[terminal] shell` — explicit shell program to spawn in the local
    /// terminal pane. `None` → auto-resolve per platform (`$SHELL` /
    /// `/bin/bash` / `/bin/sh` on Unix; `pwsh.exe` → `powershell.exe` →
    /// `cmd.exe` on Windows). Set to override, e.g. `"fish"` or
    /// `"/usr/bin/zsh"`. Passed to `term::resolve_shell` on pane open.
    pub terminal_shell: Option<String>,
    /// `[downloads] dir` — local directory where `d` (download) writes files.
    /// `None` → resolve via `dirs::download_dir()` at use time (OS-independent),
    /// falling back to `$HOME/Downloads`, then cwd. See `download_dir()`.
    pub downloads_dir: Option<PathBuf>,
    /// `[sessions] new_session_root` — directory the Sessions-mode "create
    /// workspace" picker (ADR 0014) starts browsing from. A BACKEND path (the
    /// picker lists the daemon host's filesystem, e.g. the backend host), so set it to
    /// where your projects live, e.g. `"/home/you/projects"`. `None` →
    /// the existing fallback chain (`$SOT_PROJECTS_ROOT` → `$SOT_REMOTE_HOME`
    /// → the daemon's own default row root → `$HOME`) — see
    /// `begin_create_session`'s own doc comment for the full, current
    /// tier list (a per-host hosts.toml `remote_home` field was never
    /// part of it; that key was deleted with the v1 grammar).
    pub new_session_root: Option<String>,
    /// `[font] scale` — default text-scale multiplier applied at startup when
    /// no per-host persisted zoom exists. Precedence: persisted `font_scale`
    /// (Ctrl+=/-/0, per-host state toml) > this key > the built-in
    /// monitor-width tier (wide displays default larger; see
    /// `default_font_scale_for_width`) > 1.0. Maintainer note, 2026-07-03: default was
    /// "a bit small" on big monitors.
    pub font_scale: Option<f32>,
    /// `[nav] spill_ms` — while the user is actively moving the nav cursor,
    /// rows whose text overflows the nav column float their full text over
    /// the preview pane's left edge (rendered by the overlay text layer;
    /// pane geometry never moves); the overlay vanishes this many
    /// milliseconds after the last cursor move. `0` disables the spill
    /// entirely. Default 2000.
    pub nav_spill_ms: u64,
    /// `[gpu] power_preference` — which adapter to request from wgpu.
    /// Default [`GpuPowerPreference::Low`] (integrated), which keeps the
    /// discrete GPU asleep on hybrid-graphics laptops.
    ///
    /// **Takes effect on the next frontend start.** The preference binds
    /// once, at adapter/surface creation in `State::new`, so editing this
    /// key mid-session does nothing until the frontend restarts
    /// (`scripts/relaunch-sot.ps1`, ADR 0017).
    pub gpu_power_preference: GpuPowerPreference,
    /// `[drawer] attach_only` — ADR 0041 step 6 U3: when `true`, the
    /// Terminal drawer does not spawn a local PTY at all; it resolves the
    /// per-machine state dir, reads `drawer.voyage`, and attaches to a
    /// running `sot-capsule supervise` as a watcher (Windows only — see
    /// `sot_log::attach_client::client`). Default `false`: OFF BY DEFAULT, and
    /// when off nothing the FE does today changes — the drawer keeps
    /// spawning `term::LocalTerminal` exactly as before this unit landed.
    /// Read once at drawer-creation time (not hot-reloaded mid-session,
    /// same as `gpu_power_preference` above — switching it takes a
    /// frontend restart because a live drawer's backend is not swapped
    /// under it).
    pub attach_only: bool,
    /// `[display] fullscreen_vsync_pin` — the fullscreen steady-redraw guard
    /// from commit a2907aea (#15): on a VRR/adaptive-sync OLED panel,
    /// borderless fullscreen disengages DWM composition, so the on-demand
    /// idle path's ~1 fps present drives the panel into the 1–10 Hz
    /// low-framerate-compensation band, where brightness visibly pumps.
    /// Default `false` — most panels are fixed-refresh and the pin buys
    /// them nothing. Set `true` on a VRR/adaptive-sync OLED panel that
    /// pumps brightness in borderless fullscreen: `about_to_wait` then
    /// keeps requesting redraws every vsync while fullscreen so the panel
    /// stays pinned at its native refresh. Costs 25.5 points of one core
    /// and 8.5 points of iGPU 3D continuously at idle in fullscreen
    /// (measured on one laptop). No VRR detection API is trusted here,
    /// so this is a setting, not a probe.
    pub fullscreen_vsync_pin: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            preset: PresetMode::Auto,
            ultrawide: LayoutPreset::default_ultrawide(),
            laptop: LayoutPreset::default_laptop(),
            portrait: LayoutPreset::default_portrait(),
            repl_auto_open_drawer_on_run: true,
            terminal_shell: None,
            downloads_dir: None,
            new_session_root: None,
            font_scale: None,
            nav_spill_ms: 2000,
            gpu_power_preference: GpuPowerPreference::Low,
            attach_only: false,
            fullscreen_vsync_pin: false,
        }
    }
}

impl Settings {
    /// Layered load: defaults overlaid with whatever
    /// `find_config_file` returns. A failed load logs at warn
    /// level and falls back to defaults — same contract as
    /// `KeyBindings::load_layered()`.
    pub fn load_layered() -> Self {
        let mut s = Self::default();
        if let Some(path) = find_config_file("SOT_SETTINGS", "settings.toml") {
            match fs::read_to_string(&path) {
                Ok(contents) => {
                    s.merge_text(&contents);
                    tracing::info!(path = %path.display(), "settings loaded");
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e,
                        "failed to read settings file; using defaults");
                }
            }
        }
        s
    }

    /// Resolve which `LayoutPreset` to use given the primary
    /// monitor's aspect ratio (width / height). `Auto` picks by
    /// bucket; explicit preset names ignore the aspect.
    pub fn resolve_preset(&self, aspect: f32) -> &LayoutPreset {
        match self.preset {
            PresetMode::Auto => {
                if aspect > 1.9 {
                    &self.ultrawide
                } else if aspect >= 1.5 {
                    &self.laptop
                } else {
                    &self.portrait
                }
            }
            PresetMode::Ultrawide => &self.ultrawide,
            PresetMode::Laptop => &self.laptop,
            PresetMode::Portrait => &self.portrait,
        }
    }

    /// Resolve the effective local download directory. Configured
    /// `[downloads] dir` wins; otherwise the OS download dir
    /// (`dirs::download_dir()` — cross-platform), then `$HOME/Downloads`,
    /// then the current working directory as a last resort. Always returns
    /// a path so the download path is never ambiguous; the caller creates
    /// the directory if it doesn't exist.
    pub fn download_dir(&self) -> PathBuf {
        if let Some(d) = &self.downloads_dir {
            return d.clone();
        }
        dirs::download_dir()
            .or_else(|| dirs::home_dir().map(|h| h.join("Downloads")))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Parse a settings file body on top of `self`.
    fn merge_text(&mut self, contents: &str) {
        let mut section = String::new();
        for (lineno, raw) in contents.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            if let Some(inner) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = inner.trim().to_string();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = strip_quotes(value.trim());
            match (section.as_str(), key) {
                ("layout", "preset") => {
                    if let Some(p) = PresetMode::parse(&value) {
                        self.preset = p;
                    } else {
                        tracing::warn!(line = lineno + 1, value = %value,
                            "layout.preset: expected one of auto|ultrawide|laptop|portrait");
                    }
                }
                ("layout.ultrawide", _) => merge_preset_kv(&mut self.ultrawide, key, &value, lineno),
                ("layout.laptop", _) => merge_preset_kv(&mut self.laptop, key, &value, lineno),
                ("layout.portrait", _) => merge_preset_kv(&mut self.portrait, key, &value, lineno),
                ("repl", "auto_open_drawer_on_run") => match parse_bool(&value) {
                    Some(b) => self.repl_auto_open_drawer_on_run = b,
                    None => tracing::warn!(line = lineno + 1, value = %value,
                        "repl.auto_open_drawer_on_run: expected true|false"),
                },
                ("font", "scale") => match value.trim().parse::<f32>() {
                    Ok(v) if (0.5..=3.0).contains(&v) => self.font_scale = Some(v),
                    _ => tracing::warn!(line = lineno + 1, value = %value,
                        "font.scale: expected a number in [0.5, 3.0]"),
                },
                ("nav", "spill_ms") => match value.trim().parse::<u64>() {
                    Ok(v) => self.nav_spill_ms = v,
                    Err(_) => tracing::warn!(line = lineno + 1, value = %value,
                        "nav.spill_ms: expected a non-negative integer (ms; 0 disables)"),
                },
                ("gpu", "power_preference") => match GpuPowerPreference::parse(&value) {
                    Some(p) => self.gpu_power_preference = p,
                    None => tracing::warn!(line = lineno + 1, value = %value,
                        "gpu.power_preference: expected low|high"),
                },
                ("terminal", "shell") => {
                    self.terminal_shell = parse_terminal_shell(&value);
                }
                ("downloads", "dir") => {
                    // Empty string = unset (fall back to the OS download dir).
                    let v = value.trim();
                    self.downloads_dir = if v.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(v))
                    };
                }
                ("sessions", "new_session_root") => {
                    // Empty string = unset (fall back to the env/host chain).
                    let v = value.trim();
                    self.new_session_root =
                        if v.is_empty() { None } else { Some(v.to_string()) };
                }
                ("drawer", "attach_only") => match parse_bool(&value) {
                    Some(b) => self.attach_only = b,
                    None => tracing::warn!(line = lineno + 1, value = %value,
                        "drawer.attach_only: expected true|false"),
                },
                ("display", "fullscreen_vsync_pin") => match parse_bool(&value) {
                    Some(b) => self.fullscreen_vsync_pin = b,
                    None => tracing::warn!(line = lineno + 1, value = %value,
                        "display.fullscreen_vsync_pin: expected true|false"),
                },
                // Known, and not the frontend's: the DAEMON reads
                // `[trust] root_prefix` out of the user-level copy of this
                // same file (`backend::agents::folder_trust::trusted_root_prefix`), and
                // the installer writes it there. Recognised here so a key
                // our own install writes is not reported as a typo at every
                // start; there is nothing for the frontend to do with it.
                ("trust", "root_prefix") => {}
                _ => {
                    tracing::warn!(line = lineno + 1, section = %section, key,
                        "unknown settings key; ignored");
                }
            }
        }
    }
}

fn merge_preset_kv(preset: &mut LayoutPreset, key: &str, value: &str, lineno: usize) {
    match key {
        "columns" => match parse_columns(value) {
            Some(cols) => preset.columns = cols,
            None => tracing::warn!(line = lineno + 1, value = %value,
                "columns: expected comma list of nav|preview|llm|repl"),
        },
        "widths" => match parse_widths(value) {
            Some(ws) => preset.widths = ws,
            None => tracing::warn!(line = lineno + 1, value = %value,
                "widths: expected comma list of positive fractions"),
        },
        "drawer" => {
            let v = value.trim();
            if v.is_empty() || v == "none" {
                preset.drawer = None;
            } else if let Some(s) = Slot::parse(v) {
                preset.drawer = Some(s);
            } else {
                tracing::warn!(line = lineno + 1, value = %v,
                    "drawer: expected one of nav|preview|llm|repl or none");
            }
        }
        "drawer_height" => match value.parse::<f32>() {
            Ok(v) if v.is_finite() => preset.drawer_height = v.clamp(DRAWER_MIN, DRAWER_MAX),
            _ => tracing::warn!(line = lineno + 1, value = %value,
                "drawer_height: expected fraction in [0.10, 0.80]"),
        },
        _ => tracing::warn!(line = lineno + 1, key,
            "unknown layout-preset key; ignored"),
    }
}

fn parse_columns(s: &str) -> Option<Vec<Slot>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let slot = Slot::parse(tok)?;
        if !seen.insert(slot) {
            // Duplicate column name — invalid.
            return None;
        }
        out.push(slot);
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

fn parse_widths(s: &str) -> Option<Vec<f32>> {
    let mut out = Vec::new();
    for tok in s.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let v: f32 = tok.parse().ok()?;
        if !v.is_finite() || v <= 0.0 {
            return None;
        }
        out.push(v);
    }
    if out.is_empty() {
        return None;
    }
    // Renormalise so user can write 1/6, 1/3, 1/2 (= 0.999) without
    // the final column losing 1 pixel to rounding. Sum the inputs and
    // scale each so the total is 1.0; anything truly out of whack
    // (e.g. all zeros) was already rejected above.
    let sum: f32 = out.iter().sum();
    if sum <= 0.0 {
        return None;
    }
    for w in out.iter_mut() {
        *w /= sum;
    }
    Some(out)
}

/// Parse a `[terminal] shell` value. Empty string or all-whitespace
/// is treated as "unset" (returns `None` so auto-resolution proceeds).
fn parse_terminal_shell(s: &str) -> Option<String> {
    let trimmed = s.trim().to_string();
    if trimmed.is_empty() { None } else { Some(trimmed) }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub(in crate::ui) fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
        .to_string()
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
