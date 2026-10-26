// ui/drawer/monitor.rs — frontend state + SVG rendering for the Ctrl+M server-
// monitor drawer (ADR 0020). Holds a per-host ring of recent samples (filled
// by monitor.history on open, appended by monitor.tick) and renders a
// small-multiples line chart as an SVG string. `layout_drawer_px` rasterizes that string
// through the existing resvg -> wgpu-quad path into the drawer rect — the same
// path that renders MathJax, so there is no new rendering machinery and no
// ratatui braille/sparkline hack (which the project rejects).
//
// v1 is a linear recent-window view (CPU, RAM, per-GPU utilization). The
// log/multiscale time axis + history-backed zoom is Task 5 (ADR 0020 §3).

use std::collections::{HashMap, VecDeque};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use sot_protocol::{HostLatest, HostSeries, MonitorSample, ProcSample};
use crate::ui::*;

const RING_CAP: usize = 900; // ~15 min of 1 Hz live tail
const WINDOW_S: f64 = 300.0; // default visible window (5 min)
const GAP_S: f64 = 5.0; // break the trace across gaps wider than this, and mark a host whose last reading is older (stale)

// Trace colors. CPU warm, RAM dim, GPUs a distinct palette (a shared multi-GPU host has two).
const CPU_COLOR: &str = "#f2a64d";
const RAM_COLOR: &str = "#6b7280";
const GPU_COLORS: [&str; 4] = ["#4dd0e1", "#e15dcf", "#9ccc65", "#ff8a65"];

struct HostBuf {
    samples: VecDeque<MonitorSample>,
    stale: bool,
    /// Last-known static capacity (cores, total RAM GiB). Cached because the
    /// backend may send them only on the first sample (ADR 0020 §4 ride-once).
    cpu_cores: Option<u32>,
    ram_total_gb: Option<f32>,
    /// Last-known top-by-CPU process list. Cached because only live ticks and
    /// raw-tier history carry procs (downsampled tiers drop them) — a history
    /// prefill or a momentarily proc-less sample must not blank the line.
    top_procs: Vec<ProcSample>,
    /// When this window last received a sample for the host, on its own clock: a sampler, daemon or connection that
    /// stops sending leaves the last reading on screen, and only its age says so.
    heard: Option<Instant>,
}

impl HostBuf {
    fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            stale: true,
            cpu_cores: None,
            ram_total_gb: None,
            top_procs: Vec::new(),
            heard: None,
        }
    }

    /// The mark after the host's name: empty while a reading has arrived within `GAP_S`; otherwise, or when the
    /// daemon reports the host's source down, `*` and the shown reading's age. The window's own clock decides; the
    /// age is the larger of the time since the reading arrived and its timestamp's age, so a reading shown from
    /// history reads its full age on clocks that agree.
    fn mark(&self, now: Instant, now_epoch: f64) -> String {
        let silent = self.heard.map(|t| now.saturating_duration_since(t).as_secs_f64());
        if !self.stale && silent.is_none_or(|s| s <= GAP_S) {
            return String::new();
        }
        let Some(last) = self.samples.back() else {
            return " *".to_string();
        };
        let age = silent.unwrap_or(0.0).max(now_epoch - last.ts).max(0.0) as u64;
        match age {
            0..=59 => format!(" * {age}s"),
            60..=3599 => format!(" * {}m", age / 60),
            _ => format!(" * {}h", age / 3600),
        }
    }

    fn push(&mut self, s: MonitorSample) {
        self.heard = Some(Instant::now());
        if let Some(c) = s.cpu_cores {
            self.cpu_cores = Some(c);
        }
        if let Some(g) = s.ram_total_gb {
            self.ram_total_gb = Some(g);
        }
        if !s.top_procs.is_empty() {
            self.top_procs = s.top_procs.clone();
        }
        if self.samples.len() >= RING_CAP {
            self.samples.pop_front();
        }
        self.samples.push_back(s);
    }
}

/// Per-host ring buffers + roster + subscription flag for the monitor drawer.
pub struct MonitorView {
    hosts: Vec<String>,
    bufs: HashMap<String, HostBuf>,
    /// Whether this window holds a monitor subscription: from the Monitor drawer's open to its close by the toggle. It
    /// is sent at the open and again at each new connection to the monitor host (`State::resubscribe_monitor`). Only
    /// `MonitorPane` changes it.
    subscribed: bool,
    /// What the chart was last drawn for: the drawer's size in pixels, its text scale and each host's mark; `None`
    /// before the first draw and while the drawer shows another tenant. A draw that fails to raster still records it,
    /// so a failed draw is not retried every frame. Any difference redraws it, so a mark that ages past `GAP_S` with no
    /// tick, a resize and a return to the Monitor tenant each show a true chart.
    drawn: Option<(u32, u32, f64, Vec<String>)>,
}

impl MonitorView {
    pub fn new() -> Self {
        Self {
            hosts: Vec::new(),
            bufs: HashMap::new(),
            subscribed: false,
            drawn: None,
        }
    }

    /// Each host's mark (`HostBuf::mark`) at `now` on the window's monotonic clock and `now_epoch` on its wall clock,
    /// in display order.
    fn marks(&self, now: Instant, now_epoch: f64) -> Vec<String> {
        self.hosts.iter().map(|h| self.bufs.get(h).map_or_else(String::new, |b| b.mark(now, now_epoch))).collect()
    }

    /// Roster from the monitor.subscribe response — fixes display order and
    /// lets empty panels lay out before the first tick.
    pub fn set_roster(&mut self, hosts: Vec<String>) {
        for h in &hosts {
            self.bufs.entry(h.clone()).or_insert_with(HostBuf::new);
        }
        self.hosts = hosts;
    }

    /// Append one live tick (one host). A stale tick (no sample) only flips the
    /// host's stale flag; the trace then ends, rendering a gap (ADR 0020 §5).
    pub fn apply_tick(&mut self, t: HostLatest) {
        let buf = self.bufs.entry(t.host.clone()).or_insert_with(HostBuf::new);
        buf.stale = t.stale;
        if let Some(s) = t.sample {
            buf.push(s);
        }
        if !self.hosts.iter().any(|h| h == &t.host) {
            self.hosts.push(t.host);
        }
    }

    /// Prefill from a monitor.history response (authoritative for the window).
    pub fn apply_history(&mut self, series: Vec<HostSeries>) {
        for hs in series {
            let buf = self.bufs.entry(hs.host.clone()).or_insert_with(HostBuf::new);
            buf.stale = hs.stale;
            buf.samples.clear();
            for s in hs.samples {
                buf.push(s);
            }
            if !self.hosts.iter().any(|h| h == &hs.host) {
                self.hosts.push(hs.host);
            }
        }
    }

    /// Render the small-multiples chart (one panel per host) as an SVG string
    /// sized to the drawer rect in pixels, each host's name followed by its
    /// mark from `marks` (`MonitorView::marks`, in display order).
    #[allow(clippy::too_many_lines, reason = "renders the monitor chart as one SVG; predates the 100-line limit")]
    pub fn render_svg(&self, w: u32, h: u32, scale: f64, marks: &[String]) -> String {
        let w = w.max(1) as f64;
        let h = h.max(1) as f64;
        // The chart is laid out in logical px (sizes tuned at ~1x); `scale`
        // (the chrome's text scale) lifts text + gutters so labels stay
        // legible on a hi-DPI drawer rect.
        let s = scale.max(0.5);
        let n = self.hosts.len().max(1);

        let mut out = String::with_capacity(4096);
        out.push_str(&format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{w:.0}\" height=\"{h:.0}\" viewBox=\"0 0 {w:.0} {h:.0}\">"
        ));
        out.push_str(&format!(
            "<rect x=\"0\" y=\"0\" width=\"{w:.0}\" height=\"{h:.0}\" fill=\"#101216\"/>"
        ));

        // Text sizes: logical px lifted by the chrome scale `s`, then sized up
        // so labels read at a glance on a hi-DPI drawer. Host name is largest;
        // the colored value readout next to it doubles as the legend.
        let label_fs = 22.0 * s;
        let val_fs = 17.0 * s;
        let madv = 0.62; // monospace advance / font-size, for readout layout
        let pad = 6.0 * s;
        let sw = 1.6 * s; // data-line stroke width
        let panel_h = (h - pad) / n as f64;

        for (i, host) in self.hosts.iter().enumerate() {
            let py0 = pad + i as f64 * panel_h;
            let py1 = py0 + panel_h - pad;
            let Some(buf) = self.bufs.get(host) else {
                continue;
            };

            // ── Header line: host name (bold) + colored current-value readout.
            // The readout shows absolute cpu/ram % and per-GPU util/mem %, and
            // its colors are the legend for the traces below.
            let mark = marks.get(i).map_or("", String::as_str);
            let (lcolor, lsuffix) = if mark.is_empty() {
                ("#cdd3de", "")
            } else {
                ("#e2b341", mark)
            };
            let base_y = py0 + label_fs;
            out.push_str(&format!(
                "<text x=\"{:.1}\" y=\"{base_y:.1}\" font-family=\"monospace\" font-weight=\"bold\" font-size=\"{label_fs:.1}\" fill=\"{lcolor}\">{}{lsuffix}</text>",
                pad,
                escape(host),
            ));
            if let Some(s) = buf.samples.back() {
                let mut rx =
                    pad + (host.chars().count() + lsuffix.len() + 2) as f64 * label_fs * madv;
                // Capacity first (static), percentage last and right-aligned to
                // a fixed width, so the digits of a changing % don't shift the
                // rest of the readout around.
                let cpu_txt = match buf.cpu_cores {
                    Some(c) => format!("cpu {c}c {:>3.0}%", s.cpu_pct),
                    None => format!("cpu {:>3.0}%", s.cpu_pct),
                };
                let ram_txt = match buf.ram_total_gb {
                    Some(g) => format!("ram {:.0}G {:>3.0}%", g, s.ram_pct),
                    None => format!("ram {:>3.0}%", s.ram_pct),
                };
                let mut segs: Vec<(&str, String)> = vec![
                    (CPU_COLOR, cpu_txt),
                    (RAM_COLOR, ram_txt),
                ];
                let mut gs: Vec<_> = s.gpus.iter().collect();
                gs.sort_by_key(|g| g.index);
                for g in gs {
                    let color = GPU_COLORS[g.index as usize % GPU_COLORS.len()];
                    // Unified-memory GPUs (GB10) have no discrete VRAM to
                    // report -- read "n/a" rather than a misleading "0m%".
                    let mem_txt = match g.mem_pct {
                        Some(m) => format!("{m:>3.0}m%"),
                        None => "n/a".to_string(),
                    };
                    segs.push((
                        color,
                        format!("g{} {:>3.0}u/{mem_txt}", g.index, g.util_pct),
                    ));
                }
                for (color, txt) in &segs {
                    out.push_str(&format!(
                        "<text x=\"{:.1}\" y=\"{base_y:.1}\" font-family=\"monospace\" font-size=\"{val_fs:.1}\" fill=\"{color}\">{}</text>",
                        rx,
                        escape(txt),
                    ));
                    rx += (txt.chars().count() as f64 + 1.5) * val_fs * madv;
                }
            }

            // ── Top-processes line under the header: `name·owner NN%` ×3,
            // instantaneous CPU (top-style; >100% = multithreaded). Only
            // rendered (and only takes height) when the backend sends procs,
            // so panels from an older daemon keep their full plot area.
            let mut proc_h = 0.0;
            if !buf.top_procs.is_empty() {
                proc_h = val_fs * 1.3;
                let proc_y = py0 + label_fs + proc_h;
                let txt = buf
                    .top_procs
                    .iter()
                    .map(|p| format!("{}\u{b7}{} {:.0}%", p.name, p.user, p.cpu_pct))
                    .collect::<Vec<_>>()
                    .join("  ");
                out.push_str(&format!(
                    "<text x=\"{:.1}\" y=\"{proc_y:.1}\" font-family=\"monospace\" font-size=\"{val_fs:.1}\" fill=\"#9aa3b2\">top {}</text>",
                    pad,
                    escape(&txt),
                ));
            }

            // ── Plot area below the header (full width) ──
            let plot_x0 = pad;
            let plot_x1 = (w - pad).max(plot_x0 + 1.0);
            let plot_y0 = py0 + label_fs + proc_h + pad;
            if plot_y0 + 2.0 >= py1 {
                continue; // panel too short to plot under the header
            }
            out.push_str(&format!(
                "<rect x=\"{plot_x0:.1}\" y=\"{plot_y0:.1}\" width=\"{:.1}\" height=\"{:.1}\" fill=\"none\" stroke=\"#2a2e36\" stroke-width=\"1\"/>",
                plot_x1 - plot_x0,
                (py1 - plot_y0).max(0.0)
            ));
            for frac in [0.25_f64, 0.5, 0.75] {
                let gy = py1 - frac * (py1 - plot_y0);
                out.push_str(&format!(
                    "<line x1=\"{plot_x0:.1}\" y1=\"{gy:.1}\" x2=\"{plot_x1:.1}\" y2=\"{gy:.1}\" stroke=\"#1c2026\" stroke-width=\"1\"/>"
                ));
            }

            if buf.samples.is_empty() {
                continue;
            }

            // Visible window: last WINDOW_S, or the buffer span if shorter.
            let last_ts = buf.samples.back().map(|s| s.ts).unwrap_or(0.0);
            let span = last_ts - buf.samples.front().map(|s| s.ts).unwrap_or(last_ts);
            let window = WINDOW_S.min(span.max(1.0));
            let t0 = last_ts - window;
            let xmap = |ts: f64| plot_x0 + ((ts - t0) / window).clamp(0.0, 1.0) * (plot_x1 - plot_x0);
            let ymap = |val: f64| py1 - (val.clamp(0.0, 100.0) / 100.0) * (py1 - plot_y0);

            // CPU + RAM (solid). Per-GPU: utilization solid, memory dashed in
            // the same color, so each GPU reads as a util/mem pair.
            push_series(&mut out, buf, t0, CPU_COLOR, sw, false, &xmap, &ymap, |s| {
                Some(s.cpu_pct as f64)
            });
            push_series(&mut out, buf, t0, RAM_COLOR, sw, false, &xmap, &ymap, |s| {
                Some(s.ram_pct as f64)
            });
            let maxg = buf.samples.iter().map(|s| s.gpus.len()).max().unwrap_or(0);
            for gi in 0..maxg {
                let color = GPU_COLORS[gi % GPU_COLORS.len()];
                push_series(&mut out, buf, t0, color, sw, false, &xmap, &ymap, move |s| {
                    s.gpus
                        .iter()
                        .find(|g| g.index as usize == gi)
                        .map(|g| g.util_pct as f64)
                });
                // None (unified memory: not applicable) breaks the dashed
                // trace into a gap rather than drawing a false 0% line.
                push_series(&mut out, buf, t0, color, sw, true, &xmap, &ymap, move |s| {
                    s.gpus
                        .iter()
                        .find(|g| g.index as usize == gi)
                        .and_then(|g| g.mem_pct)
                        .map(|m| m as f64)
                });
            }
        }

        out.push_str("</svg>");
        out
    }
}

impl Default for MonitorView {
    fn default() -> Self {
        Self::new()
    }
}

/// Emit a series as one or more polylines, breaking the line wherever the
/// sample gap exceeds GAP_S (a stale window) so downtime shows as a gap.
#[allow(clippy::too_many_arguments)]
fn push_series<FX, FY, FV>(
    out: &mut String,
    buf: &HostBuf,
    t0: f64,
    color: &str,
    sw: f64,
    dash: bool,
    xmap: &FX,
    ymap: &FY,
    val: FV,
) where
    FX: Fn(f64) -> f64,
    FY: Fn(f64) -> f64,
    FV: Fn(&MonitorSample) -> Option<f64>,
{
    let mut seg: Vec<(f64, f64)> = Vec::new();
    let mut prev_ts: Option<f64> = None;
    for s in buf.samples.iter() {
        if s.ts < t0 {
            prev_ts = Some(s.ts);
            continue;
        }
        if let Some(pt) = prev_ts {
            if s.ts - pt > GAP_S {
                flush_seg(out, &mut seg, color, sw, dash);
            }
        }
        if let Some(v) = val(s) {
            seg.push((xmap(s.ts), ymap(v)));
        }
        prev_ts = Some(s.ts);
    }
    flush_seg(out, &mut seg, color, sw, dash);
}

fn flush_seg(out: &mut String, seg: &mut Vec<(f64, f64)>, color: &str, sw: f64, dash: bool) {
    // GPU memory rides as a dashed line in the same color as its util pair.
    let dash_attr = if dash {
        format!(" stroke-dasharray=\"{:.1},{:.1}\"", sw * 3.0, sw * 2.5)
    } else {
        String::new()
    };
    if seg.len() >= 2 {
        let pts: Vec<String> = seg.iter().map(|(x, y)| format!("{x:.1},{y:.1}")).collect();
        out.push_str(&format!(
            "<polyline points=\"{}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"{sw:.1}\"{dash_attr}/>",
            pts.join(" ")
        ));
    } else if seg.len() == 1 {
        out.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{:.1}\" fill=\"{color}\"/>",
            seg[0].0,
            seg[0].1,
            (sw * 0.9).max(0.8)
        ));
    }
    seg.clear();
}

/// The window's monotonic clock, which decides staleness, and its wall clock in seconds, which ages a timestamp.
pub(super) fn clocks() -> (Instant, f64) {
    let epoch = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    (Instant::now(), epoch)
}

/// Text from the daemon (a host, a process or a user name) as the chart's SVG text: one row (`cell_row`), with U+FFFE
/// and U+FFFF as U+FFFD, since XML 1.0 forbids those and most control characters and usvg rejects the whole chart for
/// one, then markup escaped.
fn escape(s: &str) -> String {
    cell_row(s)
        .replace(['\u{fffe}', '\u{ffff}'], "\u{fffd}")
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The Monitor drawer (ADR 0020): its chart model, the chart rasterized for the drawer rect, that rect, whether the
/// readings changed since the last raster, the subscription this window holds and the hub it asks.
pub(in crate::ui) struct MonitorPane {
    view: MonitorView,
    /// The rasterized chart painted into the drawer rect via the resvg→wgpu-quad path (same as MathJax).
    quad: Option<Quad>,
    /// The drawer's pixel rect from the last layout pass.
    rect: ScreenRect,
    /// The readings changed since the last raster (a change of size, text scale or mark redraws by itself:
    /// `MonitorView`'s `drawn`).
    dirty: bool,
    /// The declared hub's name (topology `hub = "<host>"`), loaded once at
    /// startup the same way `selfupdate.rs` reads the topology. `None` when
    /// no `hosts.toml` declares one. Names the monitor drawer's subscribe
    /// target (`monitor_host`) — the drawer means "the fleet's record", so
    /// it asks the hub, not whichever connection sorts first.
    hub: Option<String>,
}

/// A chart the layout pass must rasterize: its SVG at `w` by `h` pixels, and what it is drawn for.
pub(in crate::ui) struct ChartDraw {
    pub(in crate::ui) svg: String,
    pub(in crate::ui) w: u32,
    pub(in crate::ui) h: u32,
    drawn_for: (u32, u32, f64, Vec<String>),
}

impl MonitorPane {
    pub(in crate::ui) fn new(hub: Option<String>) -> Self {
        Self {
            view: MonitorView::new(),
            quad: None,
            rect: ScreenRect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 },
            dirty: false,
            hub,
        }
    }

    pub(in crate::ui) fn hub(&self) -> Option<&str> { self.hub.as_deref() }
    pub(in crate::ui) fn quad(&self) -> Option<&Quad> { self.quad.as_ref() }
    pub(in crate::ui) fn rect(&self) -> ScreenRect { self.rect }

    /// Whether this window holds a monitor subscription: from the Monitor drawer's open to its close by the toggle.
    /// It is sent at the open and again at each new connection to the monitor host (`State::resubscribe_monitor`).
    pub(in crate::ui) fn subscribed(&self) -> bool { self.view.subscribed }
    /// The subscription was sent, or (`--start-monitor`) is held for the monitor host's first `Connected`.
    pub(in crate::ui) fn hold_subscription(&mut self) { self.view.subscribed = true; }
    pub(in crate::ui) fn release_subscription(&mut self) { self.view.subscribed = false; }

    pub(in crate::ui) fn set_roster(&mut self, hosts: Vec<String>) {
        self.view.set_roster(hosts);
        self.dirty = true;
    }

    pub(in crate::ui) fn apply_history(&mut self, hosts: Vec<HostSeries>) {
        self.view.apply_history(hosts);
        self.dirty = true;
    }

    pub(in crate::ui) fn apply_ticks(&mut self, hosts: Vec<HostLatest>) {
        for h in hosts {
            self.view.apply_tick(h);
        }
        self.dirty = true;
    }

    /// The layout pass: the drawer's rect, whether the Monitor is its tenant, and the chart's text scale. A chart is
    /// due when the readings changed or it was drawn for another size, text scale or set of marks, and the rect is
    /// more than a pixel each way; another tenant drops the chart and what it was drawn for.
    pub(in crate::ui) fn plan(&mut self, rect: ScreenRect, shown: bool, mon_scale: f64) -> Option<ChartDraw> {
        self.rect = rect;
        if !shown {
            self.quad = None;
            self.view.drawn = None;
            return None;
        }
        let mw = self.rect.w.max(1.0) as u32;
        let mh = self.rect.h.max(1.0) as u32;
        // One clock read per frame. A reading that ages past GAP_S changes its host's mark with no tick to say
        // so; the idle repaint brings this check round once a second.
        let (now, now_epoch) = clocks();
        let view = (mw, mh, mon_scale, self.view.marks(now, now_epoch));
        if (self.dirty || self.view.drawn.as_ref() != Some(&view)) && mw > 1 && mh > 1 {
            let svg = self.view.render_svg(mw, mh, mon_scale, &view.3);
            return Some(ChartDraw { svg, w: mw, h: mh, drawn_for: view });
        }
        None
    }

    /// The planned chart's raster (`None` when it failed, which still records what it was drawn for, so a failed
    /// draw is not retried every frame).
    pub(in crate::ui) fn install(&mut self, draw: ChartDraw, quad: Option<Quad>) {
        self.quad = quad;
        self.dirty = false;
        self.view.drawn = Some(draw.drawn_for);
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
