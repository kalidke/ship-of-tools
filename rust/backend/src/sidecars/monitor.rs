// monitor.rs — server-monitoring data plane (ADR 0020).
//
// One long-lived sampler per monitored host emits NDJSON (one line per
// interval); the local host runs `bash`, remote hosts run `ssh <alias> bash
// -s` with the script fed over stdin (nothing lands on the remote's disk).
// A supervisor task per host reads the lines, pushes each sample into a
// tiered in-memory ring (so `monitor.history` can serve any window), and
// broadcasts it as a live tick. Sampling is **always-on** for the life of the
// backend so the drawer shows real history the moment it opens; per-connection
// tick *delivery* is gated separately by `monitor.subscribe` in `server/dispatch.rs`.
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
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
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
  # nvidia-smi prints "[N/A]" (or "N/A" / "Not Supported" / empty) for a
  # field the driver has nothing to report -- notably memory.used/total on a
  # unified-memory GPU (e.g. GB10), which has no discrete VRAM to query.
  # numf() renders "null" for those instead of coercing them to 0, so a
  # not-reported field reads as absent rather than lying about the value;
  # memory stays null unless BOTH used and total are real numbers.
  gpus=$(nvidia-smi --query-gpu=index,utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw \
           --format=csv,noheader,nounits 2>/dev/null \
         | awk -F', *' 'BEGIN{ORS="";print "["}
              function isna(x) { gsub(/^[ \t]+|[ \t]+$/, "", x); return (x=="" || x=="N/A" || x=="[N/A]" || x=="Not Supported") }
              function numf(x, fmt) { return isna(x) ? "null" : sprintf(fmt, x+0) }
              { if(NR>1)print ",";
                m = (isna($3) || isna($4) || ($4+0)<=0) ? "null" : sprintf("%.1f", ($3+0)/($4+0)*100);
                printf "{\"i\":%d,\"u\":%s,\"m\":%s,\"t\":%s,\"p\":%s}",($1+0),numf($2,"%.0f"),m,numf($5,"%.0f"),numf($6,"%.1f") }
              END{print "]"}')
  [ -z "$gpus" ] && gpus="[]"
  # Top-3 processes by INSTANTANEOUS cpu: delta utime+stime per pid across
  # ticks (what `top` does), NOT ps's lifetime pcpu — a long-lived now-idle
  # process must not outrank what is hot now. First tick has no delta -> [].
  # Like top's irix mode, a multithreaded process can read > 100%.
  # cat, not `awk ... /proc/[0-9]*/stat`: a pid glob-expanded by the shell can
  # have exited by the time awk gets around to opening it, and awk treats a
  # missing ARGV file as FATAL (aborts before its END block ever runs, so the
  # whole scan silently returns nothing) -- confirmed on both gawk and mawk.
  # cat instead warns per missing file and keeps reading the rest, so one
  # dead pid among many (routine on a busy, many-process host) no longer
  # blanks the entire top-3 list.
  now_pts=$(date +%s.%N)
  pout=$(cat /proc/[0-9]*/stat 2>/dev/null | awk -v prev="$proc_prev" '
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
    }' 2>/dev/null)
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
    // Each rides as `null` (SAMPLER_SH's `numf`/`isna`) when nvidia-smi has
    // nothing to report for it -- so a not-reported field never fails the
    // whole sample the way a required f32 would on a JSON null.
    u: Option<f32>,
    m: Option<f32>,
    t: Option<f32>,
    p: Option<f32>,
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
                    // Utilization isn't seen N/A in practice (GB10 still
                    // reports 0%); a not-reported reading renders as idle
                    // rather than dropping the GPU from the sample.
                    util_pct: g.u.unwrap_or(0.0),
                    mem_pct: g.m,
                    temp_c: g.t,
                    power_w: g.p,
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
    // Memory is averaged only over the samples that actually reported it, so
    // a unified-memory GPU (always null) finalizes to None instead of a
    // false 0% -- `m_n` can be < `n` when some ticks had it and others didn't.
    m_n: u32,
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
            if let Some(m) = g.mem_pct {
                a.m += m as f64;
                a.m_n += 1;
            }
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
                        mem_pct: (a.m_n > 0).then(|| (a.m / a.m_n.max(1) as f64) as f32),
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
            tokio::spawn(supervise(h, tick_tx, rings, crate::lifecycle::child_signal::process()));
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

/// Whether a death reason is worth a fresh log line: only when it is
/// non-empty and differs from the last one logged for this host. The wire
/// already carries "still failing" on every tick via the ring's `stale`
/// flag; the journal only needs to hold the one thing the wire does not,
/// which is why.
fn should_log(prev: Option<&str>, next: &str) -> bool {
    !next.is_empty() && prev != Some(next)
}

/// `lines.next_line()`, or pending forever when there is no pipe to read.
/// Lets `supervise` `select!` over stdout and stderr uniformly even though
/// one of them may not have been captured.
async fn next_or_pending<R: AsyncBufRead + Unpin>(
    lines: &mut Option<Lines<R>>,
) -> std::io::Result<Option<String>> {
    match lines {
        Some(l) => l.next_line().await,
        None => std::future::pending().await,
    }
}

/// Per-host supervisor: spawn the sampler, stream its lines into the ring +
/// broadcast, and on death emit a stale tick and respawn after a backoff.
async fn supervise(
    host: MonitorHost,
    tick_tx: broadcast::Sender<HostLatest>,
    rings: Arc<Mutex<HashMap<String, HostRing>>>,
    sig: &'static crate::lifecycle::child_signal::Signal,
) {
    // The reason last logged for this host's death, so a target that stays
    // unreachable logs it once rather than on every ~5s respawn forever.
    let mut last_reason: Option<String> = None;
    loop {
        if sig.is_fired() {
            return;
        }
        let reason = match spawn_source(&host, sig).await {
            Ok((mut child, _contained)) => {
                let mut stdout = child.stdout.take().map(|s| BufReader::new(s).lines());
                let mut stderr = child.stderr.take().map(|s| BufReader::new(s).lines());
                // ssh's own words for why the source died, kept only until
                // the child does — one bounded string, never a growing log.
                let mut stderr_line: Option<String> = None;
                loop {
                    tokio::select! {
                        // The daemon is shutting down: the signal has already
                        // killed the sampler's tree.
                        _ = sig.fired() => return,
                        line = next_or_pending(&mut stdout) => {
                            match line {
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
                                    break stderr_line.take().unwrap_or_default();
                                }
                                Err(e) => {
                                    tracing::warn!(host = %host.name, error = %e, "monitor: sampler read error");
                                    break stderr_line.take().unwrap_or_default();
                                }
                            }
                        }
                        line = next_or_pending(&mut stderr) => {
                            if let Ok(Some(line)) = line {
                                let line = line.trim();
                                if !line.is_empty() {
                                    stderr_line = Some(line.chars().take(200).collect());
                                }
                            }
                        }
                    }
                }
                // `_contained` drops before `child`: the sampler's tree dies, then it is reaped.
            }
            Err(e) => e.to_string(),
        };
        if should_log(last_reason.as_deref(), &reason) {
            tracing::warn!(host = %host.name, reason = %reason, "monitor: sampler died");
            last_reason = Some(reason);
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
        tokio::select! {
            _ = tokio::time::sleep(RESPAWN_BACKOFF) => {}
            _ = sig.fired() => return,
        }
    }
}

/// Spawn the sampler for one host and feed it the script over stdin.
async fn spawn_source(
    host: &MonitorHost,
    sig: &'static crate::lifecycle::child_signal::Signal,
) -> std::io::Result<(tokio::process::Child, crate::lifecycle::child_signal::Contained)> {
    let interval = "1";
    #[cfg(test)]
    let stub = tests::STUB_SAMPLER.lock().unwrap().iter().find(|(h, _)| *h == host.name).map(|(_, p)| p.clone());
    #[cfg(not(test))]
    let stub: Option<String> = None;
    let mut cmd = if let Some(program) = stub {
        Command::new(program)
    } else if host.local {
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
        .stderr(Stdio::piped());

    let (mut child, contained) = sig.spawn(&mut cmd)?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(SAMPLER_SH.as_bytes()).await?;
        // Close stdin so `bash -s` hits EOF and starts executing the loop.
        let _ = stdin.shutdown().await;
    }
    Ok((child, contained))
}


// ─── Config: sampling targets from the declared topology ─────────────────

/// The sampling roster: the `[monitor]` table of `hosts.toml`, read through
/// the ONE parser (`sot_protocol::topology`; search order documented
/// there). Each entry is `<label> = "<ssh target>"`; an empty target means
/// the label. The entry whose label or target equals this box's
/// `host_name()` is sampled locally (no ssh), so one file serves whichever
/// box the daemon runs on. No file, or a file the parser rejects (logged),
/// falls back to this host alone. A non-Linux daemon then drops its own host
/// (`without_local_sampler`).
///
/// Called ONCE, from `server::serve`, before any connection is accepted.
/// The roster is fixed for the life of the daemon: editing `hosts.toml`
/// while it runs changes nothing until a restart — a deliberate limit (the
/// samplers are spawned eagerly from this list) that has bitten, so it is
/// said here.
/// The sampling roster this box actually executes. The declared monitor
/// table is HUB-SCOPED DECLARATION: only the declared hub runs it. Every
/// other daemon samples the host it runs on (on Linux) and nothing else — it is not
/// the monitoring authority, and asking it to ssh targets it may not
/// resolve produced a partial record that looked like a whole one.
fn sampling_roster(topo: &sot_protocol::topology::Topology, local: &str) -> Vec<MonitorHost> {
    if topo.hub == local {
        monitor_hosts(topo.monitor_targets(), local)
    } else {
        vec![MonitorHost { name: local.into(), ssh_alias: None, local: true }]
    }
}

pub fn load_hosts() -> Vec<MonitorHost> {
    let local = sot_log::host::state_dir::host_name().unwrap_or_else(|_| "local".to_string());
    let hosts = match sot_protocol::topology::load() {
        Ok(Some((path, topo))) => {
            let hosts = sampling_roster(&topo, &local);
            if topo.hub == local {
                tracing::info!(path = ?path, count = hosts.len(), "monitor: hosts loaded, this box is the hub");
            } else {
                tracing::info!(path = ?path, hub = %topo.hub, "monitor: not the hub; sampling only this host");
            }
            hosts
        }
        Ok(None) => Vec::new(),
        Err(e) => {
            tracing::warn!("monitor: hosts.toml rejected, monitoring local host only: {e}");
            Vec::new()
        }
    };
    let hosts = if hosts.is_empty() {
        tracing::info!(host = %local, "monitor: no [monitor] entries; monitoring local host only");
        vec![MonitorHost { name: local, ssh_alias: None, local: true }]
    } else {
        hosts
    };
    without_local_sampler(hosts, cfg!(target_os = "linux"))
}

/// The sampler is a Linux script fed to `bash -s` (`/proc`, `nvidia-smi`),
/// so only a Linux daemon samples its own host. Elsewhere it can produce
/// nothing: on Windows the only `bash` is git-bash, the daemon runs without
/// a console, and every 5-second respawn of the dying script opened a new
/// console window into the git install directory (field report,
/// 2026-09-17); on macOS there is no `/proc`, so the script loops every
/// second spawning short-lived processes for no sample. A daemon on any
/// other OS samples nothing locally; ssh targets on a hub's roster are
/// untouched. `linux_host` is a parameter so both branches are
/// unit-testable on one platform.
fn without_local_sampler(hosts: Vec<MonitorHost>, linux_host: bool) -> Vec<MonitorHost> {
    if linux_host {
        return hosts;
    }
    let kept: Vec<MonitorHost> = hosts.into_iter().filter(|h| !h.local).collect();
    tracing::info!(
        count = kept.len(),
        "monitor: not a Linux host; the local host is not sampled (the sampler is a Linux script)"
    );
    kept
}

fn monitor_hosts(targets: Vec<(String, String)>, local: &str) -> Vec<MonitorHost> {
    targets
        .into_iter()
        .map(|(name, target)| {
            let is_local = name == local || target == local;
            MonitorHost { local: is_local, ssh_alias: (!is_local).then_some(target), name }
        })
        .collect()
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
