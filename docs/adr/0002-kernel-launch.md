# ADR 0002: Kernel launch and process supervision

**Status:** Accepted
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
