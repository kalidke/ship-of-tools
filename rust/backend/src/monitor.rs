// monitor.rs — server-monitoring data plane (ADR 0020).
//
// One long-lived sampler per monitored host emits NDJSON (one line per
// interval); the local host runs `bash`, remote hosts run `ssh <alias> bash
// -s` with the script fed over stdin (nothing lands on the remote's disk).
// A supervisor task per host reads the lines, pushes each sample into a
// tiered in-memory ring (so `monitor.history` can serve any window), and
// broadcasts it as a live tick. Sampling is **always-on** for the life of the
// backend so the drawer shows real history the moment it opens; per-connection
// tick *delivery* is gated separately by `monitor.subscribe` in server.rs.
//
// Failure is visible (ADR 0020 §5): when a source dies we broadcast a `stale`
// tick and respawn after a backoff, so the frontend draws a gap, never a
// silent flatline.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sot_protocol::{GpuSample, HostLatest, HostSeries, MonitorHistoryReq, MonitorSample};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;

/// The verified sampler (see ADR 0020 §1 and the Task-1 verification on all
/// three boxes). Emits one NDJSON line per interval. Fed to `bash -s <count>
/// <interval>` over stdin; `count = 0` runs forever.
const SAMPLER_SH: &str = r##"#!/usr/bin/env bash
count="${1:-0}"
interval="${2:-1}"
prev_total=0
prev_idle=0
n=0
# Static host capacity (logical cores + total RAM in GiB), computed once.
cores=$(nproc 2>/dev/null || grep -c ^processor /proc/cpuinfo)
ramgb=$(awk '/^MemTotal:/{printf "%.0f", $2/1024/1024; exit}' /proc/meminfo)
# Per-process cpu accounting state (for the top-3 list): jiffies per second,
# last tick's per-pid utime+stime, and that tick's wall-clock time.
hz=$(getconf CLK_TCK 2>/dev/null || echo 100)
proc_prev=""
prev_pts=0
while :; do
  read -r _ u ni sy id io ir so st _ < /proc/stat
  idle=$((id + io))
  total=$((u + ni + sy + id + io + ir + so + st))
  cpu="0.0"
  if [ "$prev_total" -ne 0 ]; then
    dt=$((total - prev_total)); di=$((idle - prev_idle))
    [ "$dt" -gt 0 ] && cpu=$(awk -v dt="$dt" -v di="$di" 'BEGIN{printf "%.1f",(1-di/dt)*100}')
  fi
  prev_total=$total; prev_idle=$idle
  mt=$(awk '/^MemTotal:/{print $2; exit}' /proc/meminfo)
  ma=$(awk '/^MemAvailable:/{print $2; exit}' /proc/meminfo)
  ram=$(awk -v mt="$mt" -v ma="$ma" 'BEGIN{ if(mt>0) printf "%.1f",(1-ma/mt)*100; else printf "0.0" }')
  gpus=$(nvidia-smi --query-gpu=index,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw \
           --format=csv,noheader,nounits 2>/dev/null \
         | awk -F', *' 'BEGIN{ORS="";print "["}
              { if(NR>1)print ",";
                tot=($4+0); m=(tot>0)?($3+0)/tot*100:0;
                printf "{\"i\":%d,\"u\":%.0f,\"m\":%.1f,\"t\":%.0f,\"p\":%.1f}",($1+0),($2+0),m,($5+0),($6+0) }
              END{print "]"}')
  [ -z "$gpus" ] && gpus="[]"
  # Top-3 processes by INSTANTANEOUS cpu: delta utime+stime per pid across
  # ticks (what `top` does), NOT ps's lifetime pcpu — a long-lived now-idle
  # process must not outrank what is hot now. First tick has no delta -> [].
  # Like top's irix mode, a multithreaded process can read > 100%.
  now_pts=$(date +%s.%N)
  pout=$(awk -v prev="$proc_prev" '
    BEGIN { np = split(prev, a, " "); for (i = 1; i <= np; i++) { split(a[i], kv, ":"); p[kv[1]] = kv[2] } }
    {
      line = $0
      o = index(line, "(")
      r = match(line, /\)[^)]*$/)   # comm may contain spaces/parens; cut at the LAST ")"
      if (!o || !r) next
      comm = substr(line, o + 1, r - o - 1)
      pid = substr(line, 1, o - 2); gsub(/ /, "", pid)
      split(substr(line, r + 2), f, " ")
      t = f[12] + f[13]             # utime + stime (stat fields 14, 15)
      state = state pid ":" t " "
      if (pid in p && t > p[pid]) { d[pid] = t - p[pid]; name[pid] = comm }
    }
    END {
      print "S " state
      for (k = 1; k <= 3; k++) {
        best = ""; bd = 0
        for (pid in d) if (d[pid] > bd) { bd = d[pid]; best = pid }
        if (best == "") break
        printf "P %s %d %s\n", best, d[best], name[best]
        delete d[best]
      }
    }' /proc/[0-9]*/stat 2>/dev/null)
  dtp=$(awk -v a="$prev_pts" -v b="$now_pts" 'BEGIN { d = b - a; if (d <= 0) d = 1; print d }')
  top="["; sep=""
  while read -r tag pid ticks comm; do
    [ "$tag" = "P" ] || continue
    ownr=$(stat -c %U "/proc/$pid" 2>/dev/null || echo "?")
    pct=$(awk -v t="$ticks" -v hz="$hz" -v dt="$dtp" 'BEGIN { printf "%.1f", t / hz / dt * 100 }')
    comm=$(printf '%s' "$comm" | sed 's/\\/\\\\/g; s/"/\\"/g')
    ownr=$(printf '%s' "$ownr" | sed 's/\\/\\\\/g; s/"/\\"/g')
    top="$top$sep{\"n\":\"$comm\",\"o\":\"$ownr\",\"c\":$pct}"
    sep=","
  done <<EOF
$pout
EOF
  top="$top]"
  proc_prev=$(printf '%s\n' "$pout" | awk '/^S / { sub(/^S /, ""); print }')
  prev_pts=$now_pts
  printf '{"ts":%s,"cpu":%s,"ram":%s,"cc":%s,"rt":%s,"gpus":%s,"top":%s}\n' "$(date +%s.%N)" "$cpu" "$ram" "${cores:-0}" "${ramgb:-0}" "$gpus" "$top"
  n=$((n + 1))
  { [ "$count" -ne 0 ] && [ "$n" -ge "$count" ]; } && break
  sleep "$interval"
done
"##;

// Ring capacities per tier. Tiers are independent downsamples of the raw 1 Hz
// stream (averaged per bucket), giving the multiscale axis the drawer renders.
const BASE_INTERVAL_S: f64 = 1.0;
const CAP0: usize = 1800; // ~30 min @ 1 s
const CAP1: usize = 1440; // ~24 h  @ 1 min
const CAP2: usize = 720; //  ~30 d  @ 1 h
const BROADCAST_CAP: usize = 256;
const RESPAWN_BACKOFF: Duration = Duration::from_secs(5);

/// A monitored host: display name plus how to run the sampler on it.
#[derive(Debug, Clone)]
pub struct MonitorHost {
    pub name: String,
    /// SSH alias for remote hosts; `None`/unused when `local`.
    pub ssh_alias: Option<String>,
    /// True for the backend's own host — run `bash` locally instead of `ssh`.
    pub local: bool,
}

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

// ─── Raw sampler line (matches SAMPLER_SH's compact JSON) ────────────────

#[derive(Debug, Deserialize)]
struct RawSample {
    ts: f64,
    cpu: f32,
    ram: f32,
    #[serde(default)]
    cc: Option<u32>,
    #[serde(default)]
    rt: Option<f32>,
    #[serde(default)]
    gpus: Vec<RawGpu>,
    #[serde(default)]
    top: Vec<RawProc>,
}

#[derive(Debug, Deserialize)]
struct RawGpu {
    i: u32,
    u: f32,
    m: f32,
    t: f32,
    p: f32,
}

#[derive(Debug, Deserialize)]
struct RawProc {
    n: String,
    o: String,
    c: f32,
}

impl RawSample {
    fn into_sample(self) -> MonitorSample {
        MonitorSample {
            ts: self.ts,
            cpu_pct: self.cpu,
            ram_pct: self.ram,
            cpu_cores: self.cc.filter(|&c| c > 0),
            ram_total_gb: self.rt.filter(|&g| g > 0.0),
            gpus: self
                .gpus
                .into_iter()
                .map(|g| GpuSample {
                    index: g.i,
                    name: None,
                    util_pct: g.u,
                    mem_pct: g.m,
                    temp_c: Some(g.t),
                    power_w: Some(g.p),
                })
                .collect(),
            top_procs: self
                .top
                .into_iter()
                .map(|p| sot_protocol::ProcSample {
                    name: p.n,
                    user: p.o,
                    cpu_pct: p.c,
                })
                .collect(),
        }
    }
}

// ─── Tiered ring buffer ──────────────────────────────────────────────────

#[derive(Default, Clone)]
struct GpuAcc {
    u: f64,
    m: f64,
    t: f64,
    p: f64,
    n: u32,
}

/// Running average for one downsample bucket (per-minute or per-hour).
struct Bucket {
    id: i64,
    ts_sum: f64,
    cpu: f64,
    ram: f64,
    n: u32,
    // Static capacity carried through (last-known wins) so downsampled tiers
    // keep the cores / total-RAM readout.
    cpu_cores: Option<u32>,
    ram_total_gb: Option<f32>,
    gpus: BTreeMap<u32, GpuAcc>,
}

impl Bucket {
    fn new(id: i64) -> Self {
        Self {
            id,
            ts_sum: 0.0,
            cpu: 0.0,
            ram: 0.0,
            n: 0,
            cpu_cores: None,
            ram_total_gb: None,
            gpus: BTreeMap::new(),
        }
    }
    fn add(&mut self, s: &MonitorSample) {
        self.ts_sum += s.ts;
        self.cpu += s.cpu_pct as f64;
        self.ram += s.ram_pct as f64;
        self.n += 1;
        if s.cpu_cores.is_some() {
            self.cpu_cores = s.cpu_cores;
        }
        if s.ram_total_gb.is_some() {
            self.ram_total_gb = s.ram_total_gb;
        }
        for g in &s.gpus {
            let a = self.gpus.entry(g.index).or_default();
            a.u += g.util_pct as f64;
            a.m += g.mem_pct as f64;
            a.t += g.temp_c.unwrap_or(0.0) as f64;
            a.p += g.power_w.unwrap_or(0.0) as f64;
            a.n += 1;
        }
    }
    fn finalize(&self) -> MonitorSample {
        let n = self.n.max(1) as f64;
        MonitorSample {
            ts: self.ts_sum / n,
            cpu_pct: (self.cpu / n) as f32,
            ram_pct: (self.ram / n) as f32,
            cpu_cores: self.cpu_cores,
            ram_total_gb: self.ram_total_gb,
            gpus: self
                .gpus
                .iter()
                .map(|(idx, a)| {
                    let an = a.n.max(1) as f64;
                    GpuSample {
                        index: *idx,
                        name: None,
                        util_pct: (a.u / an) as f32,
                        mem_pct: (a.m / an) as f32,
                        temp_c: Some((a.t / an) as f32),
                        power_w: Some((a.p / an) as f32),
                    }
                })
                .collect(),
            // Instantaneous per-tick data: meaningless to average, so the
            // downsampled tiers drop it. Live ticks and tier0 carry it.
            top_procs: Vec::new(),
        }
    }
}

struct HostRing {
    tier0: VecDeque<MonitorSample>,
    tier1: VecDeque<MonitorSample>,
    tier2: VecDeque<MonitorSample>,
    acc1: Option<Bucket>,
    acc2: Option<Bucket>,
    /// True if the most recent signal from this host was a death/stale, so a
    /// fresh subscriber's first paint can already show the gap.
    stale: bool,
}

impl HostRing {
    fn new() -> Self {
        Self {
            tier0: VecDeque::with_capacity(CAP0),
            tier1: VecDeque::with_capacity(CAP1),
            tier2: VecDeque::with_capacity(CAP2),
            acc1: None,
            acc2: None,
            stale: true,
        }
    }

    fn push(&mut self, s: MonitorSample) {
        self.stale = false;
        Self::accumulate(&mut self.acc1, &mut self.tier1, &s, 60.0, CAP1);
        Self::accumulate(&mut self.acc2, &mut self.tier2, &s, 3600.0, CAP2);
        push_capped(&mut self.tier0, s, CAP0);
    }

    /// Fold a raw sample into a downsample tier: when the bucket id rolls over,
    /// finalize the previous bucket's average into the tier and start fresh.
    fn accumulate(
        acc: &mut Option<Bucket>,
        tier: &mut VecDeque<MonitorSample>,
        s: &MonitorSample,
        step: f64,
        cap: usize,
    ) {
        let id = (s.ts / step).floor() as i64;
        match acc {
            Some(b) if b.id == id => b.add(s),
            Some(b) => {
                push_capped(tier, b.finalize(), cap);
                let mut nb = Bucket::new(id);
                nb.add(s);
                *acc = Some(nb);
            }
            None => {
                let mut nb = Bucket::new(id);
                nb.add(s);
                *acc = Some(nb);
            }
        }
    }

    /// Choose the tier whose resolution matches the requested zoom: the
    /// coarsest tier whose step is still <= the ideal step (window / points),
    /// so a wide window reads the hourly/minute tiers and a narrow one reads
    /// the 1 s tier. Degrades to a finer tier when a coarser one has no data
    /// yet (early in the backend's life). Resolution-driven, not coverage-
    /// driven, so a short window early on returns the fine samples we have
    /// rather than a lone coarse bucket.
    fn pick(&self, ideal_step: f64) -> (f64, &VecDeque<MonitorSample>) {
        if ideal_step >= 3600.0 && !self.tier2.is_empty() {
            return (3600.0, &self.tier2);
        }
        if ideal_step >= 60.0 && !self.tier1.is_empty() {
            return (60.0, &self.tier1);
        }
        if !self.tier0.is_empty() {
            return (BASE_INTERVAL_S, &self.tier0);
        }
        if !self.tier1.is_empty() {
            return (60.0, &self.tier1);
        }
        (3600.0, &self.tier2)
    }

    fn query(&self, host: &str, window_s: f64, until: f64, points: u32) -> HostSeries {
        let from = until - window_s;
        let ideal_step = if points > 0 {
            window_s / points as f64
        } else {
            BASE_INTERVAL_S
        };
        let (step, buf) = self.pick(ideal_step);
        let mut samples: Vec<MonitorSample> = buf
            .iter()
            .filter(|s| s.ts >= from && s.ts <= until)
            .cloned()
            .collect();
        if points > 0 && samples.len() > points as usize {
            let stride = samples.len().div_ceil(points as usize);
            samples = samples.into_iter().step_by(stride).collect();
        }
        HostSeries {
            host: host.to_string(),
            step_s: step,
            stale: self.stale,
            samples,
        }
    }
}

fn push_capped(buf: &mut VecDeque<MonitorSample>, s: MonitorSample, cap: usize) {
    if buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(s);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: f64, cpu: f32) -> MonitorSample {
        MonitorSample {
            ts,
            cpu_pct: cpu,
            ram_pct: 10.0,
            cpu_cores: None,
            ram_total_gb: None,
            gpus: Vec::new(),
            top_procs: Vec::new(),
        }
    }

    #[test]
    fn raw_sample_parses_top_procs() {
        let line = r#"{"ts":1.0,"cpu":5.0,"ram":10.0,"cc":8,"rt":32,"gpus":[],"top":[{"n":"julia","o":"alice","c":187.5}]}"#;
        let s = serde_json::from_str::<RawSample>(line).unwrap().into_sample();
        assert_eq!(s.top_procs.len(), 1);
        assert_eq!(s.top_procs[0].name, "julia");
        assert_eq!(s.top_procs[0].user, "alice");
        assert!((s.top_procs[0].cpu_pct - 187.5).abs() < 0.01);
    }

    #[test]
    fn downsampled_bucket_drops_top_procs() {
        let mut r = HostRing::new();
        for i in 0..61 {
            let mut s = sample(1020.0 + i as f64, 30.0);
            s.top_procs = vec![sot_protocol::ProcSample {
                name: "julia".into(),
                user: "alice".into(),
                cpu_pct: 99.0,
            }];
            r.push(s);
        }
        assert_eq!(r.tier1.len(), 1);
        assert!(r.tier1[0].top_procs.is_empty(), "instantaneous data must not survive averaging");
        assert!(!r.tier0[0].top_procs.is_empty(), "raw tier keeps procs");
    }

    #[test]
    fn push_finalizes_minute_bucket_with_average() {
        let mut r = HostRing::new();
        // Bucket 17 spans ts 1020..1079; fill it, then cross to 1080 to roll over.
        for i in 0..60 {
            r.push(sample(1020.0 + i as f64, 30.0));
        }
        r.push(sample(1080.0, 30.0)); // crosses the 60 s boundary -> finalize bucket 17
        assert_eq!(r.tier1.len(), 1, "one minute bucket should have finalized");
        assert!((r.tier1[0].cpu_pct - 30.0).abs() < 0.01, "bucket averages cpu");
        assert!(r.tier0.len() >= 60, "tier0 keeps the raw samples");
    }

    #[test]
    fn query_returns_fine_samples_when_window_exceeds_data_span() {
        // Regression for the tier-selection bug: a window far wider than the
        // stored data must still return the fine 1 s samples we have, not a lone
        // coarse minute-bucket. The samples cross a minute boundary so tier1
        // holds a bucket — the exact condition that surfaced the bug on a shared multi-GPU host.
        let mut r = HostRing::new();
        for i in 0..=10 {
            r.push(sample(1075.0 + i as f64, 50.0)); // 1075..1085, crosses 1080
        }
        assert!(!r.tier1.is_empty(), "a minute bucket should have finalized");
        let hs = r.query("h", 120.0, 1085.0, 20); // window 120 s >> ~10 s of data
        assert_eq!(hs.step_s, 1.0, "must pick the 1 s tier, not the coarse bucket");
        assert!(
            hs.samples.len() >= 10,
            "should return the fine samples, got {}",
            hs.samples.len()
        );
    }

    #[test]
    fn query_picks_minute_tier_for_wide_window() {
        let mut r = HostRing::new();
        for i in 0..=10 {
            r.push(sample(1075.0 + i as f64, 50.0));
        }
        // window 3600 s / 20 points -> ideal step 180 s -> the minute tier.
        let hs = r.query("h", 3600.0, 1085.0, 20);
        assert_eq!(hs.step_s, 60.0);
    }

    #[test]
    fn query_strides_down_to_points() {
        let mut r = HostRing::new();
        for i in 0..100 {
            r.push(sample(2000.0 + i as f64, 50.0));
        }
        let hs = r.query("h", 200.0, 2099.0, 10);
        assert!(hs.samples.len() <= 10, "strided to <= points, got {}", hs.samples.len());
        assert!(!hs.samples.is_empty());
    }
}

// ─── Hub ─────────────────────────────────────────────────────────────────

/// Owns the broadcast bus + the per-host rings, and (on `start`) spawns one
/// always-on supervisor task per host. Cloned cheaply (Arc inside) so the
/// server can hand it to every connection and to the op handlers.
#[derive(Clone)]
pub struct MonitorHub {
    tick_tx: broadcast::Sender<HostLatest>,
    rings: Arc<Mutex<HashMap<String, HostRing>>>,
    hosts: Arc<Vec<MonitorHost>>,
}

impl MonitorHub {
    pub fn start(hosts: Vec<MonitorHost>) -> Self {
        let (tick_tx, _rx) = broadcast::channel::<HostLatest>(BROADCAST_CAP);
        let mut map = HashMap::new();
        for h in &hosts {
            map.insert(h.name.clone(), HostRing::new());
        }
        let rings = Arc::new(Mutex::new(map));
        let hub = Self {
            tick_tx,
            rings,
            hosts: Arc::new(hosts),
        };
        for h in hub.hosts.iter().cloned() {
            let tick_tx = hub.tick_tx.clone();
            let rings = hub.rings.clone();
            tokio::spawn(supervise(h, tick_tx, rings));
        }
        hub
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HostLatest> {
        self.tick_tx.subscribe()
    }

    pub fn host_names(&self) -> Vec<String> {
        self.hosts.iter().map(|h| h.name.clone()).collect()
    }

    /// Serve a history window from the rings. `host = None` returns every host.
    pub fn history(&self, req: &MonitorHistoryReq) -> Vec<HostSeries> {
        let until = req.until.unwrap_or_else(now_epoch);
        let window = req.window_s.max(1.0);
        let points = req.points;
        let rings = self.rings.lock().unwrap();
        let mut out = Vec::new();
        for h in self.hosts.iter() {
            if let Some(only) = &req.host {
                if &h.name != only {
                    continue;
                }
            }
            if let Some(ring) = rings.get(&h.name) {
                out.push(ring.query(&h.name, window, until, points));
            }
        }
        out
    }
}

/// Per-host supervisor: spawn the sampler, stream its lines into the ring +
/// broadcast, and on death emit a stale tick and respawn after a backoff.
async fn supervise(
    host: MonitorHost,
    tick_tx: broadcast::Sender<HostLatest>,
    rings: Arc<Mutex<HashMap<String, HostRing>>>,
) {
    loop {
        match spawn_source(&host).await {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() {
                    let mut lines = BufReader::new(stdout).lines();
                    loop {
                        match lines.next_line().await {
                            Ok(Some(line)) => {
                                let line = line.trim();
                                if line.is_empty() {
                                    continue;
                                }
                                match serde_json::from_str::<RawSample>(line) {
                                    Ok(raw) => {
                                        let sample = raw.into_sample();
                                        if let Ok(mut rings) = rings.lock() {
                                            if let Some(r) = rings.get_mut(&host.name) {
                                                r.push(sample.clone());
                                            }
                                        }
                                        let _ = tick_tx.send(HostLatest {
                                            host: host.name.clone(),
                                            stale: false,
                                            sample: Some(sample),
                                        });
                                    }
                                    Err(e) => {
                                        tracing::warn!(host = %host.name, error = %e, line, "monitor: bad sampler line");
                                    }
                                }
                            }
                            Ok(None) => {
                                tracing::warn!(host = %host.name, "monitor: sampler stdout closed");
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(host = %host.name, error = %e, "monitor: sampler read error");
                                break;
                            }
                        }
                    }
                }
                // child dropped here -> kill_on_drop reaps it.
            }
            Err(e) => {
                tracing::warn!(host = %host.name, error = %e, "monitor: sampler spawn failed");
            }
        }
        // Source is down: surface a gap, mark the ring stale, back off, retry.
        if let Ok(mut rings) = rings.lock() {
            if let Some(r) = rings.get_mut(&host.name) {
                r.stale = true;
            }
        }
        let _ = tick_tx.send(HostLatest {
            host: host.name.clone(),
            stale: true,
            sample: None,
        });
        tokio::time::sleep(RESPAWN_BACKOFF).await;
    }
}

/// Spawn the sampler for one host and feed it the script over stdin.
async fn spawn_source(host: &MonitorHost) -> std::io::Result<tokio::process::Child> {
    let interval = "1";
    let mut cmd = if host.local {
        let mut c = Command::new("bash");
        c.arg("-s").arg("0").arg(interval);
        c
    } else {
        let alias = host.ssh_alias.as_deref().unwrap_or(&host.name);
        let mut c = Command::new("ssh");
        c.arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ServerAliveInterval=15")
            .arg("-o")
            .arg("ServerAliveCountMax=3")
            .arg(alias)
            .arg("bash")
            .arg("-s")
            .arg("0")
            .arg(interval);
        c
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = cmd.spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(SAMPLER_SH.as_bytes()).await?;
        // Close stdin so `bash -s` hits EOF and starts executing the loop.
        let _ = stdin.shutdown().await;
    }
    Ok(child)
}

// ─── Config: monitored host list from hosts.toml ─────────────────────────

/// Load the monitored host list. Looks for a `[monitor]` table in the same
/// `hosts.toml` the frontend uses (layered candidate paths). Each entry is
/// `<display-name> = "<ssh-alias>"`, where `"local"` or `""` marks the
/// backend's own host. Falls back to monitoring just this host.
///
/// `project_root` is the daemon's `--project-root`: the repo-rooted config
/// layer resolves against it, NOT `current_dir()`, because the daemon's cwd is
/// the user's `$HOME`, not the repo.
pub fn load_hosts(project_root: &std::path::Path) -> Vec<MonitorHost> {
    let local = local_host_name();
    for path in candidate_paths(project_root) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let hosts = parse_monitor_section(&text, &local);
            if !hosts.is_empty() {
                tracing::info!(path = ?path, count = hosts.len(), "monitor: hosts loaded");
                return hosts;
            }
        }
    }
    tracing::info!(host = %local, "monitor: no [monitor] config; monitoring local host only");
    vec![MonitorHost {
        name: local,
        ssh_alias: None,
        local: true,
    }]
}

fn local_host_name() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .split('.')
        .next()
        .unwrap_or("local")
        .to_string()
}

fn parse_monitor_section(text: &str, local: &str) -> Vec<MonitorHost> {
    let mut out = Vec::new();
    let mut in_monitor = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(sec) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            in_monitor = sec.trim() == "monitor";
            continue;
        }
        if !in_monitor {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let name = k.trim().to_string();
        let alias_raw = strip_quotes(v.trim());
        // Empty alias falls back to the host name. A host is sampled locally
        // (no SSH) when its name or alias matches this machine's hostname, so
        // the same config works whichever box the backend runs on.
        let alias = if alias_raw.is_empty() { name.as_str() } else { alias_raw };
        let is_local = name == local || alias == local;
        out.push(MonitorHost {
            local: is_local,
            ssh_alias: if is_local { None } else { Some(alias.to_string()) },
            name,
        });
    }
    out
}

fn strip_quotes(s: &str) -> &str {
    let b = s.as_bytes();
    if b.len() >= 2 && b[0] == b'"' && b[b.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Layered discovery, highest priority first. Mirrors the frontend's host
/// registry (`frontend/src/hosts.rs`, ADR 0026). The repo-rooted layer resolves
/// against `project_root` rather than `current_dir()` — the daemon's cwd is
/// `$HOME`, so a cwd-relative `.sot/hosts.toml` silently missed the repo's
/// config and the monitor fell back to local-host-only.
///   1. $SOT_HOSTS (explicit override)
///   2. <project-root>/.sot/hosts.toml
///   3. $XDG_CONFIG_HOME/sot/hosts.toml
///   4. $HOME/.config/sot/hosts.toml
///   5. %APPDATA%/sot/hosts.toml (Windows)
fn candidate_paths(project_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let mut out = Vec::new();
    if let Some(p) = std::env::var_os("SOT_HOSTS") {
        out.push(PathBuf::from(p));
    }
    out.push(project_root.join(".sot").join("hosts.toml"));
    if let Some(p) = std::env::var_os("XDG_CONFIG_HOME") {
        let base = PathBuf::from(p);
        out.push(base.join("sot").join("hosts.toml"));
    }
    if let Some(p) = std::env::var_os("HOME") {
        let cfg = PathBuf::from(p).join(".config");
        out.push(cfg.join("sot").join("hosts.toml"));
    }
    if let Some(p) = std::env::var_os("APPDATA") {
        let base = PathBuf::from(p);
        out.push(base.join("sot").join("hosts.toml"));
    }
    out
}
