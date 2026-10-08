# ADR 0002: Kernel launch and process supervision

**Status:** current — accepted.
**Shutdown superseded (0.6.6):** every process the daemon starts, but those ADR 0050 names as outside it, starts through `Signal::spawn` or `spawn_std` (rust/backend/src/lifecycle/child_signal.rs; the platform calls are in rust/backend/src/lifecycle/contain.rs) in its own process group (Unix) or job (Windows). The tree is killed with SIGKILL to the group or with TerminateJobObject when its owner lets go or the shutdown fires (the exits that do not fire it are ADR 0050 known limit (p)). There is no SIGTERM grace and no taskkill.
**0.6.6 sidecar wiring:** REPL and MathJax now receive the caller's Signal through construction, spawn and their supervisor loop, as the other sidecar supervisors do. This changes injection and ownership wiring, not the production process-signal shutdown policy.
**0.6.6 REPL restart:** Explicit project restart sends an owned stop, closes in-flight work, checks contained-tree termination and direct-child reap, and joins before publishing a new child. REPL_RESTART_WAIT bounds the asynchronous retirement/join wait to 5 seconds; it is not a hard bound on lock acquisition or OS operations. A failed retirement remains owned and prevents replacement. The selected project survives subsequent death respawn. This states the current explicit-restart behavior; automatic respawn-policy redesign remains outside this change.
**Date:** 2026-05-07

## Context

The Rust backend supervises two long-lived Julia child processes: the kernel (plugin host, project introspector) and the REPL (user's interactive session). Both must work cleanly on Windows and Linux. Orphaned `julia.exe` after a crash is unacceptable.

## Decision

`tokio::process::Command` spawns:

```
julia --project=<repo>/julia/kernel -e 'using ShipToolsKernel; ShipToolsKernel.serve(stdin, stdout)'
```

stdio is the transport (per ADR 0001). stderr is captured into a backend log ring buffer. `kill_on_drop(true)`.

Restart policy:
- **Kernel** — auto-restart on exit, exponential backoff (1s, 2s, 4s, 8s, 16s, then surface to UI). State rebuilds from disk; safe to restart.
- **REPL** — never auto-restarts. A crashed REPL is meaningful information; user decides whether to restart.

Shutdown:
- **Linux** — SIGTERM, wait 5s, then SIGKILL.
- **Windows** — `taskkill /F /T /PID <pid>`. Do not rely on SIGTERM semantics; tokio's `kill()` alone leaves grandchildren.

## Consequences

- M1 acceptance includes verifying no orphaned `julia.exe` after Ctrl-C, `taskkill`, and task-manager kill paths on Windows.
- Backend log ring buffer is the *only* place kernel stderr surfaces; user-facing errors must be sent as protocol events, not printed to stderr.
- REPL non-restart means UI must clearly indicate "REPL is dead, press X to restart" — not silently respawn.
