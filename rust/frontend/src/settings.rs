// settings.rs — user-configurable layout (and future general) settings
// for the frontend.
//
// Sibling to keybindings.rs and following the same layered-discovery
// pattern, so both files feel consistent and the user (or the LLM
// editing on their behalf) only has to learn one shape.
//
// File discovery, in priority order:
//   1. $SOT_SETTINGS  — explicit override path
//   2. <repo-root>/.sot/settings.toml  — project-level
//   3. $HOME/.config/sot/settings.toml  — user-level
//
// File format (ADR 0014's layout rework — aspect-ratio-keyed presets,
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
//   resume_command = "claude --continue"
//                                   # Auto-run in the Terminal drawer when
//                                   # the supervisor respawns us after a
//                                   # self-relaunch (--relaunched, ADR 0017).
//                                   # Omit to use the default shown.
//
//   [downloads]
//   dir = "/home/me/sot-downloads" # Local directory that `d` (download)
//                                   # writes files into. Omit to use the OS
//                                   # download dir (Win %USERPROFILE%\Downloads,
//                                   # Linux XDG_DOWNLOAD_DIR/~/Downloads, mac
//                                   # ~/Downloads), falling back to cwd.
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
use std::path::{Path, PathBuf};

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
    /// `[terminal] resume_command` — command auto-run in the Terminal drawer
    /// when the frontend is started with `--relaunched` (i.e. the supervisor
    /// respawned us after a self-relaunch, ADR 0017). Lets a `claude` session
    /// that triggered the rebuild reattach itself in the fresh process.
    /// `None` → fall back to the built-in default `claude --continue`.
    pub terminal_resume_command: Option<String>,
    /// `[downloads] dir` — local directory where `d` (download) writes files.
    /// `None` → resolve via `dirs::download_dir()` at use time (OS-independent),
    /// falling back to `$HOME/Downloads`, then cwd. See `download_dir()`.
    pub downloads_dir: Option<PathBuf>,
    /// `[sessions] new_session_root` — directory the Sessions-mode "create
    /// workspace" picker (ADR 0014) starts browsing from. A BACKEND path (the
    /// picker lists the daemon host's filesystem, e.g. the backend host), so set it to
    /// where your projects live, e.g. `"/home/you/projects"`. `None` →
    /// the existing fallback chain (`$SOT_PROJECTS_ROOT` → host `remote_home` →
    /// `$HOME`). See `begin_create_session`.
    pub new_session_root: Option<String>,
    /// `[font] scale` — default text-scale multiplier applied at startup when
    /// no per-host persisted zoom exists. Precedence: persisted `font_scale`
    /// (Ctrl+=/-/0, per-host state toml) > this key > the built-in
    /// monitor-width tier (wide displays default larger; see
    /// `default_font_scale_for_width`) > 1.0. Maintainer note, 2026-07-03: default was
    /// "a bit small" on big monitors.
    pub font_scale: Option<f32>,
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
            terminal_resume_command: None,
            downloads_dir: None,
            new_session_root: None,
            font_scale: None,
        }
    }
}

impl Settings {
    /// Layered load: defaults overlaid with whatever
    /// `find_settings_file()` returns. A failed load logs at warn
    /// level and falls back to defaults — same contract as
    /// `KeyBindings::load_layered()`.
    pub fn load_layered() -> Self {
        let mut s = Self::default();
        if let Some(path) = find_settings_file() {
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
                ("terminal", "shell") => {
                    self.terminal_shell = parse_terminal_shell(&value);
                }
                ("terminal", "resume_command") => {
                    // Same empty-is-unset treatment as `shell`.
                    self.terminal_resume_command = parse_terminal_shell(&value);
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

fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| s.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(s)
        .to_string()
}

fn find_settings_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SOT_SETTINGS") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut cur: &Path = &cwd;
        loop {
            let candidate = cur.join(".sot").join("settings.toml");
            if candidate.is_file() {
                return Some(candidate);
            }
            match cur.parent() {
                Some(parent) => cur = parent,
                None => break,
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home)
            .join(".config")
            .join("sot")
            .join("settings.toml");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_auto_with_three_columns_each_preset() {
        let s = Settings::default();
        assert_eq!(s.preset, PresetMode::Auto);
        assert_eq!(
            s.ultrawide.columns,
            vec![Slot::Nav, Slot::Preview, Slot::Llm]
        );
        assert_eq!(s.ultrawide.widths.len(), 3);
        let sum: f32 = s.ultrawide.widths.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "widths must sum to ~1.0");
    }

    #[test]
    fn auto_resolves_by_aspect() {
        let s = Settings::default();
        // 32:9 / 21:9 → ultrawide.
        assert_eq!(s.resolve_preset(2.4).columns, s.ultrawide.columns);
        // 16:10 → laptop.
        assert_eq!(s.resolve_preset(1.6).columns, s.laptop.columns);
        // 4:3 → portrait.
        assert_eq!(s.resolve_preset(1.33).columns, s.portrait.columns);
    }

    #[test]
    fn explicit_preset_ignores_aspect() {
        let mut s = Settings::default();
        s.preset = PresetMode::Laptop;
        assert_eq!(s.resolve_preset(3.0).columns, s.laptop.columns);
    }

    #[test]
    fn parse_columns_accepts_named_slots() {
        assert_eq!(
            parse_columns("nav,preview,llm"),
            Some(vec![Slot::Nav, Slot::Preview, Slot::Llm])
        );
        assert_eq!(parse_columns("NAV, PREVIEW"), Some(vec![Slot::Nav, Slot::Preview]));
    }

    #[test]
    fn parse_columns_rejects_duplicates_and_unknown() {
        assert!(parse_columns("nav,nav").is_none());
        assert!(parse_columns("nav,unknown,llm").is_none());
        assert!(parse_columns("").is_none());
    }

    #[test]
    fn parse_widths_renormalises_to_one() {
        let ws = parse_widths("0.167,0.333,0.5").unwrap();
        let sum: f32 = ws.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
        // Even if the user writes integers, fractions are derived.
        let ws = parse_widths("1,2,3").unwrap();
        let sum: f32 = ws.iter().sum();
        assert!((sum - 1.0).abs() < 1e-4);
        assert!((ws[2] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn parse_widths_rejects_zeros_and_negatives() {
        assert!(parse_widths("0,1").is_none());
        assert!(parse_widths("-0.5,0.5").is_none());
    }

    #[test]
    fn merge_overrides_preset_and_subsection() {
        let mut s = Settings::default();
        s.merge_text(
            "[layout]\npreset = \"laptop\"\n\n[layout.laptop]\ncolumns = \"nav,llm\"\nwidths = \"0.25,0.75\"\ndrawer = \"none\"\n",
        );
        assert_eq!(s.preset, PresetMode::Laptop);
        assert_eq!(s.laptop.columns, vec![Slot::Nav, Slot::Llm]);
        assert!((s.laptop.widths[0] - 0.25).abs() < 1e-3);
        assert!(s.laptop.drawer.is_none());
    }

    #[test]
    fn repl_auto_open_drawer_defaults_true_and_parses() {
        assert!(Settings::default().repl_auto_open_drawer_on_run);
        let mut s = Settings::default();
        s.merge_text("[repl]\nauto_open_drawer_on_run = false\n");
        assert!(!s.repl_auto_open_drawer_on_run);
        s.merge_text("[repl]\nauto_open_drawer_on_run = \"on\"\n");
        assert!(s.repl_auto_open_drawer_on_run);
        // Garbage value leaves the prior setting untouched.
        s.merge_text("[repl]\nauto_open_drawer_on_run = banana\n");
        assert!(s.repl_auto_open_drawer_on_run);
    }

    #[test]
    fn merge_keeps_defaults_on_garbage() {
        let mut s = Settings::default();
        let before = s.ultrawide.widths.clone();
        s.merge_text("[layout.ultrawide]\nwidths = \"banana,1\"\n");
        assert_eq!(s.ultrawide.widths, before);
    }

    #[test]
    fn merge_clamps_drawer_height() {
        let mut s = Settings::default();
        s.merge_text("[layout.laptop]\ndrawer_height = \"0.99\"\n");
        assert!((s.laptop.drawer_height - DRAWER_MAX).abs() < 1e-3);
        s.merge_text("[layout.laptop]\ndrawer_height = \"0.01\"\n");
        assert!((s.laptop.drawer_height - DRAWER_MIN).abs() < 1e-3);
    }

    #[test]
    fn comments_and_blank_lines_skipped() {
        let mut s = Settings::default();
        s.merge_text(
            "# header comment\n\n[layout]\n# inline note\npreset = \"ultrawide\"  # trailing\n",
        );
        assert_eq!(s.preset, PresetMode::Ultrawide);
    }

    #[test]
    fn terminal_shell_defaults_none_and_parses() {
        // Default is None (auto-resolve).
        assert!(Settings::default().terminal_shell.is_none());

        // Explicit shell is stored.
        let mut s = Settings::default();
        s.merge_text("[terminal]\nshell = \"/usr/bin/fish\"\n");
        assert_eq!(s.terminal_shell.as_deref(), Some("/usr/bin/fish"));

        // Quoted value with double-quotes.
        let mut s = Settings::default();
        s.merge_text("[terminal]\nshell = \"pwsh.exe\"\n");
        assert_eq!(s.terminal_shell.as_deref(), Some("pwsh.exe"));

        // Empty string clears to None.
        let mut s = Settings::default();
        s.merge_text("[terminal]\nshell = \"\"\n");
        assert!(s.terminal_shell.is_none());
    }

    #[test]
    fn terminal_resume_command_defaults_none_and_parses() {
        assert!(Settings::default().terminal_resume_command.is_none());
        let mut s = Settings::default();
        s.merge_text("[terminal]\nresume_command = \"claude --continue\"\n");
        assert_eq!(s.terminal_resume_command.as_deref(), Some("claude --continue"));
        // Empty clears to None (→ caller uses the built-in default).
        let mut s = Settings::default();
        s.merge_text("[terminal]\nresume_command = \"\"\n");
        assert!(s.terminal_resume_command.is_none());
    }

    #[test]
    fn downloads_dir_defaults_none_and_parses() {
        // Default is None (resolve via the OS download dir at use time).
        assert!(Settings::default().downloads_dir.is_none());

        // Explicit dir is stored verbatim.
        let mut s = Settings::default();
        s.merge_text("[downloads]\ndir = \"/home/me/dl\"\n");
        assert_eq!(s.downloads_dir, Some(PathBuf::from("/home/me/dl")));

        // Empty string clears to None.
        let mut s = Settings::default();
        s.merge_text("[downloads]\ndir = \"\"\n");
        assert!(s.downloads_dir.is_none());
    }

    #[test]
    fn download_dir_prefers_configured_then_falls_back() {
        // Configured dir wins outright.
        let mut s = Settings::default();
        s.merge_text("[downloads]\ndir = \"/tmp/sot-dl\"\n");
        assert_eq!(s.download_dir(), PathBuf::from("/tmp/sot-dl"));

        // Unconfigured falls back to *some* absolute dir (OS download dir or
        // $HOME/Downloads); on a headless CI box without either it lands on
        // cwd ("."). Just assert it never panics and returns non-empty.
        let s = Settings::default();
        let resolved = s.download_dir();
        assert!(!resolved.as_os_str().is_empty());
    }
}
