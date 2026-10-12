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
    /// the `State` methods in this file change it.
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
fn clocks() -> (Instant, f64) {
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

impl State {
    /// The toggle's half of the Monitor subscription's lifecycle (ADR 0020): a drawer toggled to the Monitor takes the
    /// subscription, and one toggled to other content or closed releases it. Help, and a REPL run that takes the slot
    /// without the toggle, keep it. Sampling is always on; the subscription gates only this connection's ticks.
    pub(in crate::ui) fn sync_monitor_subscription(&mut self) {
        let shown = self.drawer == DrawerContent::Monitor;
        if shown && !self.monitor_view.subscribed {
            self.subscribe_monitor();
        } else if !shown && self.monitor_view.subscribed {
            self.unsubscribe_monitor();
        }
    }

    /// `--start-monitor`: the drawer opens at construction holding its subscription, which the monitor host's first
    /// Connected sends (`resubscribe_monitor`), as every later one does.
    pub(in crate::ui) fn hold_monitor_subscription(&mut self) {
        self.monitor_view.subscribed = true;
    }

    /// Subscribe to the monitor host's ticks and prefill the chart from its history (ADR 0020). ADR 0042 L2a: always
    /// `monitor_host`, the declared hub; the drawer never follows `active_host`.
    fn subscribe_monitor(&mut self) {
        let monitor_host = self.monitor_host();
        let _ = self.send_to(&monitor_host, OutgoingReq::MonitorSubscribe);
        let history = OutgoingReq::MonitorHistory { window_s: 300.0, points: 300, until: None, host: None };
        let _ = self.send_to(&monitor_host, history);
        self.monitor_view.subscribed = true;
    }

    /// Release the subscription: the monitor host stops sending this connection ticks.
    fn unsubscribe_monitor(&mut self) {
        let monitor_host = self.monitor_host();
        let _ = self.send_to(&monitor_host, OutgoingReq::MonitorUnsubscribe);
        self.monitor_view.subscribed = false;
    }

    /// The daemon starts every connection unsubscribed, so a new connection to the monitor host is sent the
    /// subscription this window holds; without it the drawer would get no reading until it was reopened.
    pub(in crate::ui) fn resubscribe_monitor(&mut self, connected: &HostKey) {
        if self.monitor_view.subscribed && *connected == self.monitor_host() {
            self.subscribe_monitor();
        }
    }

    pub(crate) fn on_monitor_subscribed(&mut self, hosts: Vec<String>) {
        self.monitor_view.set_roster(hosts);
        self.monitor_dirty = true;
        self.window.request_redraw();
    }

    pub(crate) fn on_monitor_history(&mut self, hosts: Vec<sot_protocol::HostSeries>) {
        self.monitor_view.apply_history(hosts);
        self.monitor_dirty = true;
        self.window.request_redraw();
    }

    pub(crate) fn on_monitor_tick(&mut self, hosts: Vec<sot_protocol::HostLatest>) {
        for h in hosts {
            self.monitor_view.apply_tick(h);
        }
        self.monitor_dirty = true;
        self.window.request_redraw();
    }

    pub(in crate::ui) fn layout_drawer_px(
        &mut self,
        cells_to_px: impl Fn(ratatui::layout::Rect) -> ScreenRect,
        repl_scrollback_cells: ratatui::layout::Rect,
        repl_window: (usize, usize),
        repl_cells: ratatui::layout::Rect,
    ) {
        // Ctrl+M monitor drawer (ADR 0020): the chart shares the drawer rect.
        // Regenerate the SVG → wgpu quad whenever the readings changed
        // (`monitor_dirty`) or the chart on screen was drawn for another size,
        // text scale or set of marks (`MonitorView::drawn`, `None` once the
        // drawer shows another tenant), mirroring the math-SVG rasterise path.
        // Build into a local first so the immutable
        // `&self.device/queue/quad_pipeline` borrows don't collide with the
        // `self.monitor_quad` write.
        self.repl_scrollback_px = cells_to_px(repl_scrollback_cells);
        self.repl_window = repl_window;
        self.monitor_rect_px = cells_to_px(repl_cells);
        if self.drawer == DrawerContent::Monitor {
            let mw = self.monitor_rect_px.w.max(1.0) as u32;
            let mh = self.monitor_rect_px.h.max(1.0) as u32;
            // Scale the chart's text + gutters to match the chrome's
            // effective text size (cell_h is BASE_CELL_H * scale), so the
            // SVG's logical-px labels aren't tiny on a hi-DPI window.
            let mon_scale = (self.cell_h / BASE_CELL_H) as f64;
            // One clock read per frame. A reading that ages past GAP_S changes its host's mark with no tick to say
            // so; the idle repaint brings this check round once a second.
            let (now, now_epoch) = clocks();
            let view = (mw, mh, mon_scale, self.monitor_view.marks(now, now_epoch));
            if (self.monitor_dirty || self.monitor_view.drawn.as_ref() != Some(&view)) && mw > 1 && mh > 1 {
                let svg = self.monitor_view.render_svg(mw, mh, mon_scale, &view.3);
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
                self.monitor_view.drawn = Some(view);
            }
        } else {
            self.monitor_quad = None;
            self.monitor_view.drawn = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sot_protocol::{GpuSample, HostLatest, HostSeries, MonitorSample};

    /// Each host's mark on the window's clocks now.
    fn marks_now(v: &MonitorView) -> Vec<String> {
        let (now, now_epoch) = clocks();
        v.marks(now, now_epoch)
    }

    fn sample(ts: f64, cpu: f32, gpus: &[(u32, f32)]) -> MonitorSample {
        MonitorSample {
            ts,
            cpu_pct: cpu,
            ram_pct: 10.0,
            cpu_cores: Some(32),
            ram_total_gb: Some(128.0),
            gpus: gpus
                .iter()
                .map(|(i, u)| GpuSample {
                    index: *i,
                    name: None,
                    util_pct: *u,
                    mem_pct: Some(0.0),
                    temp_c: None,
                    power_w: None,
                })
                .collect(),
            top_procs: Vec::new(),
        }
    }

    #[test]
    fn render_svg_wellformed_with_per_host_traces() {
        let mut v = MonitorView::new();
        v.set_roster(vec!["host-a".into(), "host-b".into()]);
        v.apply_history(vec![
            HostSeries {
                host: "host-a".into(),
                step_s: 1.0,
                stale: false,
                samples: vec![
                    sample(100.0, 50.0, &[(0, 5.0)]),
                    sample(101.0, 60.0, &[(0, 7.0)]),
                    sample(102.0, 55.0, &[(0, 6.0)]),
                ],
            },
            HostSeries {
                host: "host-b".into(),
                step_s: 1.0,
                stale: false,
                samples: vec![
                    sample(100.0, 1.0, &[(0, 0.0), (1, 59.0)]),
                    sample(101.0, 2.0, &[(0, 0.0), (1, 60.0)]),
                    sample(102.0, 1.5, &[(0, 0.0), (1, 58.0)]),
                ],
            },
        ]);
        let svg = v.render_svg(800, 240, 1.0, &marks_now(&v));
        assert!(svg.starts_with("<svg"), "must be an svg root");
        assert!(svg.trim_end().ends_with("</svg>"), "must close the svg");
        assert!(svg.contains("host-a") && svg.contains("host-b"), "host labels present");
        // host-a: cpu+ram+gpu0 = 3 polylines; host-b: cpu+ram+gpu0+gpu1 = 4 => >=7.
        let polylines = svg.matches("<polyline").count();
        assert!(polylines >= 7, "expected >=7 polylines, got {polylines}");
    }

    #[test]
    fn stale_tick_flags_host_without_appending() {
        let mut v = MonitorView::new();
        v.apply_tick(HostLatest {
            host: "host-a".into(),
            stale: false,
            sample: Some(sample(1.0, 10.0, &[])),
        });
        v.apply_tick(HostLatest {
            host: "host-a".into(),
            stale: true,
            sample: None,
        });
        let svg = v.render_svg(400, 100, 1.0, &marks_now(&v));
        // Stale marker " *" rides next to the host label (ADR 0020 §5).
        assert!(svg.contains("host-a *"), "stale host should be marked");
    }

    /// A host whose readings stop arriving is marked stale with the shown reading's age once no reading has come for
    /// `GAP_S`: one that was live, and one prefilled from history whose last reading is an hour old. A new reading
    /// clears the mark. The silence is a moved clock, so the test waits for nothing.
    #[test]
    fn a_reading_that_stops_arriving_is_marked_stale_with_its_age() {
        let (start, epoch) = clocks();
        let mut v = MonitorView::new();
        v.set_roster(vec!["live-a".into(), "hung-b".into()]);
        v.apply_tick(HostLatest { host: "live-a".into(), stale: false, sample: Some(sample(epoch, 10.0, &[])) });
        v.apply_history(vec![HostSeries {
            host: "hung-b".into(),
            step_s: 1.0,
            stale: false,
            samples: vec![sample(epoch - 3600.0, 20.0, &[])],
        }]);
        let label = |v: &MonitorView, (now, now_epoch): (Instant, f64), host: &str| {
            let svg = v.render_svg(800, 240, 1.0, &v.marks(now, now_epoch));
            let at = svg.find(&format!(">{host}")).map_or(0, |i| i + 1);
            svg[at..].split('<').next().unwrap_or("").to_string()
        };
        let silent = GAP_S + 0.5;
        let later = (start + std::time::Duration::from_secs_f64(silent), epoch + silent);
        let mut seen = vec![label(&v, (start, epoch), "live-a"), label(&v, (start, epoch), "hung-b")];
        seen.extend([label(&v, later, "live-a"), label(&v, later, "hung-b")]);
        v.apply_tick(HostLatest { host: "live-a".into(), stale: false, sample: Some(sample(later.1, 11.0, &[])) });
        seen.push(label(&v, clocks(), "live-a"));
        eprintln!("monitor-stale: {seen:?}");
        let right = seen[0] == "live-a"
            && seen[1] == "hung-b"
            && seen[2].starts_with("live-a * ")
            && seen[2].ends_with('s')
            && seen[3] == "hung-b * 1h"
            && seen[4] == "live-a";
        assert!(right, "fresh, after {GAP_S}s silent, after a new reading: {seen:?}");
    }

    #[test]
    fn top_procs_render_and_survive_procless_samples() {
        let mut v = MonitorView::new();
        let mut s = sample(1.0, 50.0, &[]);
        s.top_procs = vec![
            sot_protocol::ProcSample {
                name: "julia".into(),
                user: "alice".into(),
                cpu_pct: 312.0,
            },
            sot_protocol::ProcSample {
                name: "nv(idia)".into(),
                user: "root".into(),
                cpu_pct: 3.0,
            },
        ];
        v.apply_tick(HostLatest {
            host: "host-a".into(),
            stale: false,
            sample: Some(s),
        });
        // A later sample without procs (e.g. a downsampled history refill)
        // must not blank the cached line.
        v.apply_tick(HostLatest {
            host: "host-a".into(),
            stale: false,
            sample: Some(sample(2.0, 51.0, &[])),
        });
        let svg = v.render_svg(800, 240, 1.0, &marks_now(&v));
        assert!(svg.contains("julia\u{b7}alice 312%"), "proc entry should render");
        assert!(svg.contains("nv(idia)\u{b7}root 3%"), "second proc should render");
    }

    #[test]
    fn a_control_character_in_a_name_leaves_the_chart_drawable() {
        // A process name is the file name it ran from, which may hold any byte but `/` and NUL; usvg rejects the
        // whole chart for one character XML 1.0 forbids (a control character, U+FFFE, U+FFFF), so a name is drawn as
        // one row (`cell_row`) with those two as U+FFFD.
        let mut v = MonitorView::new();
        let mut s = sample(1.0, 50.0, &[]);
        s.top_procs = vec![
            sot_protocol::ProcSample {
                name: "AA\u{1}BB\nCC".into(),
                user: "alice".into(),
                cpu_pct: 3.0,
            },
            sot_protocol::ProcSample {
                name: "DD\u{fffe}EE\u{ffff}".into(),
                user: "bob".into(),
                cpu_pct: 2.0,
            },
        ];
        v.apply_tick(HostLatest {
            host: "host-a".into(),
            stale: false,
            sample: Some(s),
        });
        let svg = v.render_svg(800, 240, 1.0, &marks_now(&v));
        let parsed = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default());
        assert!(parsed.is_ok(), "the chart does not parse: {:?}", parsed.err());
        assert!(svg.contains("AA BB ⏎ CC\u{b7}alice 3%"), "{svg}");
        assert!(svg.contains("DD\u{fffd}EE\u{fffd}\u{b7}bob 2%"), "{svg}");
    }

    #[test]
    fn empty_view_still_renders_a_frame() {
        let v = MonitorView::new();
        let svg = v.render_svg(200, 80, 1.0, &[]);
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
    }
}

/// The Monitor's native State fixture: a `State` exists only inside a native window, so it runs in the
/// window_minimized target's fixtures child (`ui/app/native_children.rs`).
#[cfg(all(test, feature = "test-window-progress"))]
mod native_fixture {
    use super::*;
    use crate::net::transport::{IncomingEvt, ResolvedDial};

    impl State {
        /// A new connection to the monitor host is sent the subscription this window holds, with its history prefill:
        /// with the drawer open, and with Help shown over it. No other host's connection is sent one, and the hub's
        /// is sent none once the drawer has closed.
        #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
        pub(in crate::ui) fn a_held_monitor_subscription_is_sent_on_each_new_connection(&mut self) -> Result<()> {
            let (hub, other) = ("<monitor-hub>".to_string(), "<host-other>".to_string());
            let (hub_tx, mut hub_rx) = tokio::sync::mpsc::unbounded_channel();
            let (other_tx, mut other_rx) = tokio::sync::mpsc::unbounded_channel();
            // The hub sorts second: only the declared hub, not the first connection, makes it the monitor host.
            self.conns = vec![(other.clone(), other_tx), (hub.clone(), hub_tx)];
            self.monitor_hub = Some(hub.clone());
            self.active_host = other.clone();
            let (events, receiver) = std::sync::mpsc::channel();
            self.evt_rx = receiver;
            let rows = [
                ("open, the hub connects", DrawerContent::Monitor, true, &hub),
                ("help over it, the hub connects", DrawerContent::Help, true, &hub),
                ("open, another host connects", DrawerContent::Monitor, true, &other),
                ("closed, the hub connects", DrawerContent::Closed, false, &hub),
            ];
            let mut seen = Vec::new();
            for (label, drawer, held, host) in rows {
                self.drawer = drawer;
                self.monitor_view.subscribed = held;
                let connected = IncomingEvt::Connected {
                    session_id: "session".into(),
                    revision: 0,
                    host: None,
                    project_root: None,
                    proxy: false,
                    resolved: ResolvedDial::Local,
                    backend_version: "test".into(),
                };
                events
                    .send((host.clone(), connected))
                    .map_err(|_| anyhow::anyhow!("the fixture's event channel closed"))?;
                self.drain_events();
                seen.push((label, monitor_ops(&mut hub_rx), monitor_ops(&mut other_rx)));
            }
            self.drawer = DrawerContent::Closed;
            self.monitor_view.subscribed = false;
            self.monitor_hub = None;
            println!("monitor-resubscribe: {seen:?}");
            let resent = |i: usize| seen[i].1 == ["subscribe", "history"];
            let right = resent(0)
                && resent(1)
                && seen[2].1.is_empty()
                && seen[3].1.is_empty()
                && seen.iter().all(|(_, _, to_other)| to_other.is_empty());
            anyhow::ensure!(right, "(row, monitor ops to the hub, to another host): {seen:?}");
            Ok(())
        }

        /// The chart on screen is redrawn whenever what it was drawn for changes with no new reading, as when the
        /// hub has stalled: on a resize, and on the drawer's return to the Monitor from Help.
        #[allow(dead_code, reason = "runs only inside a native State, from the window_minimized fixtures child")]
        pub(in crate::ui) fn a_monitor_chart_is_redrawn_on_resize_and_on_return(&mut self) -> Result<()> {
            let (_, epoch) = clocks();
            let reading = MonitorSample {
                ts: epoch - 3630.0,
                cpu_pct: 10.0,
                ram_pct: 10.0,
                cpu_cores: None,
                ram_total_gb: None,
                gpus: Vec::new(),
                top_procs: Vec::new(),
            };
            self.monitor_view = MonitorView::new();
            let series = HostSeries { host: "gpu-box".into(), step_s: 1.0, stale: true, samples: vec![reading] };
            self.monitor_view.apply_history(vec![series]);
            let rect = |w: f32, h: f32| move |_: ratatui::layout::Rect| ScreenRect { x: 0.0, y: 0.0, w, h };
            let cells = ratatui::layout::Rect::default();
            let rows = [
                ("drawn", DrawerContent::Monitor, (640.0, 240.0)),
                ("resized", DrawerContent::Monitor, (800.0, 300.0)),
                ("help over it", DrawerContent::Help, (800.0, 300.0)),
                ("back from help", DrawerContent::Monitor, (800.0, 300.0)),
            ];
            let mut seen = Vec::new();
            for (label, drawer, (w, h)) in rows {
                self.drawer = drawer;
                self.layout_drawer_px(rect(w, h), cells, (0, 0), cells);
                seen.push((label, self.monitor_quad.as_ref().map(|q| q.size_px)));
            }
            self.drawer = DrawerContent::Closed;
            self.monitor_quad = None;
            self.monitor_view = MonitorView::new();
            println!("monitor-redraw: {seen:?}");
            let right = seen[0].1 == Some((640, 240))
                && seen[1].1 == Some((800, 300))
                && seen[2].1.is_none()
                && seen[3].1 == Some((800, 300));
            anyhow::ensure!(right, "(row, the chart's size on screen): {seen:?}");
            Ok(())
        }
    }

    /// The monitor requests queued on one connection's channel, in order.
    fn monitor_ops(rx: &mut tokio::sync::mpsc::UnboundedReceiver<OutgoingReq>) -> Vec<&'static str> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|req| match req {
                OutgoingReq::MonitorSubscribe => Some("subscribe"),
                OutgoingReq::MonitorHistory { .. } => Some("history"),
                OutgoingReq::MonitorUnsubscribe => Some("unsubscribe"),
                _ => None,
            })
            .collect()
    }
}
