# ADR 0020: Server monitoring — SSH-poll data plane, native drawer view

**Status:** Accepted (2026-06-06). Data plane revised the same day from Netdata to a roll-our-own SSH poll once the target hosts had no passwordless sudo — see §1. Netdata remains the documented upgrade path (Out of scope).
**Date:** 2026-06-06

## Context

We want to watch GPU(s) and CPU **history** across one or more remote hosts — all at once, over a usefully long time range, in a drawer that opens with `Ctrl+M` exactly like the Julia REPL (`Ctrl+J`) and Terminal (`Ctrl+T`) drawers. The user invited using **external tools for the boring part** (collection, retention, downsampling) where they earn their keep.

Four facts from the codebase and environment shape the design:

1. **No stats collection exists.** Nothing runs `nvidia-smi` or samples `/proc`. But there *is* a clean streaming template: a backend broadcast bus → per-client `evt` frames, which is how live REPL output reaches the frontend (`repl.frame`; `rust/backend/src/repl.rs`, `server.rs` connection loop). The backend already supervises long-lived line-emitting children (kernel, REPL) over stdio.
2. **The frontend talks to one backend at a time** over a single SSH-forwarded socket; host switch = quit + relaunch (ADR 0015). So "see all servers at once" **must be solved at the data layer** — the backend you're attached to becomes the aggregator.
3. **The render stack is primitive-complete for charts.** SVG → `resvg` rasterize → wgpu textured quad is an existing path (it renders MathJax; `rust/frontend/src/preview/svg.rs`, `quad.rs`). ratatui `Chart`/`Sparkline`/`Canvas` are deliberately unused — braille/cell plotting is the same class of degraded hack as sixel/halfblocks, which the project rejects (ADR 0003, ADR 0011).
4. **No passwordless sudo; SSH keys work.** The target hosts may be remote or multi-user. `sudo` can prompt for a password, but key-based SSH from the primary dev host to the others is non-interactive.

The split is at the **collection vs. rendering** seam: an external/low-friction mechanism owns the data plane; we own the view. Collection options weighed: Netdata (system or user-space) and a roll-our-own SSH poll. Netdata's tiered storage *is* a multiscale axis for free — but every Netdata variant means a **persistent daemon on each target host**, and the system variant is blocked on sudo + admin coordination. Given fact 4, **roll-our-own SSH poll chosen** (user-directed): zero install, zero sudo, zero persistent footprint on remote hosts, using the keys that already work. `nvidia-smi` and `/proc` are world-readable, so no privileges are needed. We trade Netdata's free retention for a small, bounded amount of our own ring-buffer + downsampling code.

## Decision

### 1. Data plane: roll-our-own SSH poll

A single **sampler script** (bash, emitting one NDJSON line per interval: `{ts, cpu, ram, gpus:[{i,u,m,t,p}]}`) is the unit of collection. CPU% comes from `/proc/stat` deltas, RAM% from `/proc/meminfo`, GPU from `nvidia-smi --query-gpu=…`. The script is **embedded in the backend** and run per host:

- **myhost** (the backend's own host): spawned locally (`bash -c`).
- **host-b, host-c**: spawned as `ssh <alias> bash -s`, with the script fed over **stdin** — so nothing is written to the children's disks. Zero footprint, no daemon, no admin coordination on remote hosts.

Each source is a long-lived child whose stdout the backend reads line-by-line (same shape as the kernel/REPL supervisors), parses into a `HostSample`, tags with the host, and publishes. The backend keeps an in-memory **tiered ring buffer** per host (e.g. tier0 ~minutes @1s, tier1 ~hours @1min, tier2 ~days @1hr, downsampled as samples age) so `monitor.history` can serve any window — our own small multiscale store. SSH aliases come from `.devenv/hosts.toml` (it already carries `ssh_alias` per host); the monitored set is a `[monitor]` host list (defaults to all configured hosts).

### 2. Bridge: the connected backend aggregates, reactively

New `monitor.*` protocol ops (in `rust/protocol`, shipped). On `monitor.subscribe` (drawer opened), the backend ensures the sampler sources are running and streams `monitor.tick` evts (one fresh sample per host) onto a broadcast bus → per-client `evt` frames — the same bus→evt pattern as `repl.frame`. `monitor.history` serves a window from the ring buffer (initial fill and every time-axis rescale go through the same path). The sampler sources are **reactive**: spawned on first subscribe, torn down when the last subscriber closes the drawer ("reactive over eager"). Metrics travel over SSH stdio + an in-process broadcast, not over a shared home directory or other file transport.

### 3. View: native `Monitor` drawer, SVG traces

- A `Monitor` variant joins `DrawerContent` (`Closed`/`Repl`/`Terminal`) in `rust/frontend/src/gpu.rs`. `Ctrl+M` joins the same global toggle tier as `Ctrl+J`/`Ctrl+T`, so it works even when another drawer has focus. The native winit window distinguishes `Ctrl+M` from `Enter` (the `0x0D` collision is a *terminal* convention, irrelevant here) — match the **physical** key `KeyCode::KeyM` + Control.
- The drawer is **content-bearing**: it holds a live ring buffer plus the fetched history. It draws by **generating an SVG** (small-multiples — one compact CPU+GPU panel per host; the dual-GPU host shows both cards as separate traces) and rasterizing it through the existing `resvg` → wgpu-quad pipeline, pointed at the drawer rect. No new rendering machinery — SVG generation + drawer wiring only.
- **Log / multiscale time axis is a pure frontend redraw**: toggling log-time, switching to stacked multi-resolution panels, or zooming the range regenerates the SVG from buffered points with **no backend round-trip**. Wider/coarser windows than the live buffer are fetched once via `monitor.history` from the backend's ring tiers.

### 4. Default metrics

GPU utilization % + GPU memory % (per GPU), CPU % + RAM %, per host. Temperature/power are already in the sampler line and the protocol (`GpuSample.temp_c`/`power_w`, optional), so surfacing them later is view-only work.

### 5. Failure is visible, never quiet

A host whose SSH connection or sampler dies, or that returns stale/garbled data, renders an explicit gap/marker in its panel — never a silent flatline or a fallback to "looks fine." The `monitor.tick` / `HostSeries` types carry an explicit `stale` flag for this (project rule: no quiet fallback to a degraded path).

## Consequences

- **Positive:** nothing to install and no sudo anywhere; zero persistent footprint on remote hosts (no daemon, script over stdin); uses SSH keys that already work; rendering is on-philosophy (real vector traces); collection is reactive; no shared-filesystem metrics transport; the log/multiscale axis is interactive without round-trips.
- **Negative:** we own the ring buffer + downsampling (a bounded amount of code Netdata would have given free); history is **in-memory**, so it resets on backend restart (acceptable for v1; on-disk persistence is deferred); one long-lived SSH process per child while the drawer is open; adds SSH-from-backend (new, but trivial with working keys).

## Verification

- **Sampler:** run the script on myhost and via `ssh host-b|host-c bash -s` — each emits parseable NDJSON with sane CPU/RAM and per-GPU util/mem (host-b: 2 GPUs; host-c: a low-end GPU may report `N/A` temp/power — handle gracefully).
- **Protocol/backend:** `monitor.subscribe` starts the sources; `monitor.tick`s stream; `monitor.history` returns a downsampled window; closing the drawer stops the sources.
- **Frontend (Windows `--capture`, ≥10s):** `Ctrl+M` opens the drawer; small-multiples render for all hosts; log-time toggle redraws from the buffer with no fetch; killing a child's SSH shows a gap, not a flatline.

## Out of scope / deferred

- **Netdata** (user-space or system) as a later upgrade for durable, long-term tiered retention without owning storage — the protocol (`monitor.*`) is source-agnostic, so this is a backend-only swap.
- On-disk persistence of history across backend restarts.
- Per-process GPU/CPU attribution; alerting / thresholds.
- Monitoring a backend host that can't SSH to the others.
- Live host-switch (still relaunch-based per ADR 0015).
