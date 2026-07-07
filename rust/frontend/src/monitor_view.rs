// monitor_view.rs — frontend state + SVG rendering for the Ctrl+M server-
// monitor drawer (ADR 0020). Holds a per-host ring of recent samples (filled
// by monitor.history on open, appended by monitor.tick) and renders a
// small-multiples line chart as an SVG string. gpu.rs rasterizes that string
// through the existing resvg -> wgpu-quad path into the drawer rect — the same
// path that renders MathJax, so there is no new rendering machinery and no
// ratatui braille/sparkline hack (which the project rejects).
//
// v1 is a linear recent-window view (CPU, RAM, per-GPU utilization). The
// log/multiscale time axis + history-backed zoom is Task 5 (ADR 0020 §3).

use std::collections::{HashMap, VecDeque};

use sot_protocol::{HostLatest, HostSeries, MonitorSample, ProcSample};

const RING_CAP: usize = 900; // ~15 min of 1 Hz live tail
const WINDOW_S: f64 = 300.0; // default visible window (5 min)
const GAP_S: f64 = 5.0; // break the trace across gaps wider than this (stale)

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
}

impl HostBuf {
    fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            stale: true,
            cpu_cores: None,
            ram_total_gb: None,
            top_procs: Vec::new(),
        }
    }
    fn push(&mut self, s: MonitorSample) {
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
    /// Whether we've sent monitor.subscribe for the currently-open drawer.
    pub subscribed: bool,
}

impl MonitorView {
    pub fn new() -> Self {
        Self {
            hosts: Vec::new(),
            bufs: HashMap::new(),
            subscribed: false,
        }
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

    pub fn has_data(&self) -> bool {
        self.bufs.values().any(|b| !b.samples.is_empty())
    }

    /// Render the small-multiples chart (one panel per host) as an SVG string
    /// sized to the drawer rect in pixels.
    pub fn render_svg(&self, w: u32, h: u32, scale: f64) -> String {
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
            let (lcolor, lsuffix) = if buf.stale {
                ("#e2b341", " *")
            } else {
                ("#cdd3de", "")
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
                    segs.push((
                        color,
                        format!("g{} {:>3.0}u/{:>3.0}m%", g.index, g.util_pct, g.mem_pct),
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
                push_series(&mut out, buf, t0, color, sw, true, &xmap, &ymap, move |s| {
                    s.gpus
                        .iter()
                        .find(|g| g.index as usize == gi)
                        .map(|g| g.mem_pct as f64)
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

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sot_protocol::{GpuSample, HostLatest, HostSeries, MonitorSample};

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
                    mem_pct: 0.0,
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
        assert!(v.has_data());
        let svg = v.render_svg(800, 240, 1.0);
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
        let svg = v.render_svg(400, 100, 1.0);
        // Stale marker " *" rides next to the host label (ADR 0020 §5).
        assert!(svg.contains("host-a *"), "stale host should be marked");
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
        let svg = v.render_svg(800, 240, 1.0);
        assert!(svg.contains("julia\u{b7}alice 312%"), "proc entry should render");
        assert!(svg.contains("nv(idia)\u{b7}root 3%"), "second proc should render");
    }

    #[test]
    fn empty_view_still_renders_a_frame() {
        let v = MonitorView::new();
        assert!(!v.has_data());
        let svg = v.render_svg(200, 80, 1.0);
        assert!(svg.starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
    }
}
