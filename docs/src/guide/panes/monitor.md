# The Monitor Drawer

*Bottom drawer — `Ctrl+M`.* The Monitor shows **CPU / GPU / RAM history across all
your hosts at once** — small-multiples, one compact panel per host (a multi-GPU
box shows each GPU as its own trace). It shares the bottom drawer slot with the
[REPL](repl.md) (`Ctrl+J`) and the [Terminal](terminal.md) (`Ctrl+T`); the toggle
is global, so it opens even when another pane has focus.

![Monitor drawer with per-host resource panels](../../assets/screenshots/monitor-drawer.png)
*The Monitor drawer: CPU / GPU / RAM small-multiples, one panel per host.*

## How it gets the data

The connected backend is the **aggregator** (the frontend talks to one backend at
a time, so "see all servers at once" is solved at the data layer). For each
monitored host it runs a small sampler:

- **its own host** locally; **remote hosts** as `ssh <alias> bash -s`, with the
  script fed over stdin — **zero footprint**, no daemon, nothing written to the
  remote's disk.
- CPU% from `/proc/stat`, RAM% from `/proc/meminfo`, GPU from `nvidia-smi`. All
  world-readable, so **no sudo and no privileges** are needed.

Sampling is **always on** for the life of the backend, so the drawer shows real
history the moment it opens rather than starting from a blank axis; what
`monitor.subscribe` gates is per-connection *delivery* of ticks, not collection.
(ADR 0020 originally specified reactive spawn-on-subscribe; the implementation
went the other way and the ADR carries a note.) The backend keeps an in-memory
tiered ring buffer per host, so the time axis can rescale to wider windows
without a round-trip. Restarting the backend restarts that history.

Which hosts appear comes from the `[monitor]` section of `.sot/hosts.toml` — see
[Configuration Files](../../ref/config.md). With no `[monitor]` section, only the
backend's own host is sampled; the `[host.*]` entries are frontend connection
targets and are not monitored implicitly.

**The list binds when the daemon starts.** Editing `hosts.toml` while the backend
is running changes nothing until it restarts — there is no reload and no file
watch. If the drawer is missing a host you just added, restart the backend.

## On-philosophy rendering

Traces are drawn as **real SVG**, rasterized through the same `resvg` → wgpu-quad
pipeline that renders typeset math — not braille/cell plotting, which is the same
class of degraded hack the project rejects for images. A host whose SSH or sampler
dies renders an explicit **gap**, never a silent flatline or a quiet fallback to
"looks fine."

## See also

- [Configuration Files](../../ref/config.md) — the `[monitor]` host list in `hosts.toml`.
- [The REPL](repl.md) and [The Terminal](terminal.md) — the other two drawer contents.
