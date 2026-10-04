// the monitor drawer's samples and subscriptions

use super::*;

// ─── Server monitoring (ADR 0020) ───────────────────────────────────────

/// One GPU's metrics within a sample. `index` is the GPU's ordinal on its host
/// (a shared multi-GPU host has two). `name` rides only when known — typically the first
/// sample of a series — to spare the wire on every subsequent point. `temp_c`
/// and `power_w` are optional: the data plane (Netdata already collects them)
/// can start populating them with no protocol change (ADR 0020 §4 — util + mem
/// are the defaults, temp/power are one field away).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuSample {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub util_pct: f32,
    /// `None` when the GPU has nothing to report -- e.g. a unified-memory
    /// GPU (no discrete VRAM), which nvidia-smi reports as N/A rather than 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_c: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_w: Option<f32>,
}

/// One process in a sample's top-by-CPU list: command name, owning user, and
/// its CPU share. `cpu_pct` is instantaneous (utime+stime delta across the
/// sampler's tick interval, the same accounting `top` uses) and, like top's
/// irix mode, can exceed 100 for a multithreaded process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcSample {
    pub name: String,
    pub user: String,
    pub cpu_pct: f32,
}

/// A single observation: host CPU + RAM at `ts`, plus one entry per GPU. `ts`
/// is epoch seconds (f64 to allow sub-second cadence). The host is not repeated
/// here — it is carried by the enclosing `HostSeries` / `HostLatest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSample {
    pub ts: f64,
    pub cpu_pct: f32,
    pub ram_pct: f32,
    /// Host CPU logical core count and total RAM in GiB. Static per host, so
    /// the backend may send them on every sample or only the first; the
    /// frontend caches the last-known value and renders the percentage with
    /// its absolute capacity (e.g. `32c 10%`, `128G 3%`). Optional so an
    /// older backend that omits them still renders the bare percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_cores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ram_total_gb: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<GpuSample>,
    /// Top processes by CPU at sample time (typically 3). Live ticks carry
    /// them; downsampled history buckets drop them (instantaneous data does
    /// not average). Defaulted so older backends interop unchanged.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top_procs: Vec<ProcSample>,
}

/// A downsampled history window for one host. `step_s` is the resolution the
/// series came back at (which Netdata tier), so the frontend can label the axis
/// and decide whether to request finer/coarser data on rescale. `stale` marks a
/// host that returned no data for the window — the frontend renders a gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSeries {
    pub host: String,
    pub step_s: f64,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub samples: Vec<MonitorSample>,
}

/// `monitor.subscribe` — start this connection's live metrics stream. The
/// initial window fill is a separate `monitor.history` call (keeps subscribe
/// pure lifecycle), so the same path serves the first paint and every rescale.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSubscribeReq {
    /// Desired tick cadence in seconds; clamped backend-side to what Netdata
    /// actually produces (>= 1s).
    #[serde(default = "default_interval_s")]
    pub interval_s: f64,
}

/// `monitor.subscribe` response — echoes the cadence the backend will actually
/// stream at (after clamping) so the frontend can size its live ring buffer,
/// plus the host roster (in display order) so empty panels can be laid out
/// before the first tick arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorSubscribeRes {
    pub interval_s: f64,
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// `monitor.unsubscribe` — stop this connection's live stream. Empty payload;
/// response is a bare ack (`{}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MonitorUnsubscribeReq {}

/// `monitor.history` — fetch a window for one or all hosts from the Netdata
/// parent's tiers, downsampled to ~`points`. `until` defaults to now; the
/// window spans `window_s` back from it. `host = None` returns every host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorHistoryReq {
    pub window_s: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<f64>,
    #[serde(default = "default_points")]
    pub points: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorHistoryRes {
    pub hosts: Vec<HostSeries>,
}

/// One host's latest sample inside a `monitor.tick`. `sample` is absent when
/// the host went unreachable for the interval (`stale = true`) — the frontend
/// advances the axis and draws a gap rather than holding the last value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostLatest {
    pub host: String,
    #[serde(default)]
    pub stale: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<MonitorSample>,
}

/// Payload of a `monitor.tick` evt — one fresh sample per host, pushed at the
/// subscribed cadence (ADR 0020 §2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorTickEvt {
    pub hosts: Vec<HostLatest>,
}

fn default_interval_s() -> f64 {
    1.0
}

fn default_points() -> u32 {
    240
}
