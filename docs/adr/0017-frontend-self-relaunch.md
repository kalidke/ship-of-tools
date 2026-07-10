# ADR 0017: Frontend self-relaunch — staged-copy supervisor, sentinel trigger, terminal resume

**Status:** Accepted
**Date:** 2026-05-26

## Context

The frontend now hosts a local terminal (ADR 0016), and the user wants to dogfood DevEnv *from inside DevEnv* — drive development (including a `claude` session) in the Terminal drawer, rebuild the frontend, and have it relaunch into the new binary without dropping to an external terminal. That requires the app to restart itself.

Three Windows-specific constraints shaped the design:

1. **A running `.exe` cannot be overwritten on Windows.** The frontend launched directly from `rust\target\release\sot-frontend.exe`, so `cargo build --release` would fail the link step while the app was live.
2. **There is no `exec()` on Windows.** Relaunch is spawn-new-then-exit, which briefly contends for the window/GPU surface and socket unless something sequences it.
3. **The relaunch kills the terminal that triggered it.** The Ctrl+T shell is a direct child of the frontend (ConPTY requires the handle stay alive). Anything running in the pane — including a `claude` session — dies with the frontend.

A key user decision resolved the hardest constraint: **the terminal session does *not* need to survive a relaunch**, because `claude --continue` resumes the session from its own store. Continuity is decoupled from process survival, so we avoid building a persistent/reattachable PTY host (a Windows mini-tmux) and the tension that would create with ADR 0016's "local frontend-child PTY" decision.

## Decision

### 1. Staged-copy execution + supervisor respawn loop

`launch-devenv.ps1` copies `target\release\sot-frontend.exe` to `%LOCALAPPDATA%\devenv\bin\` and runs the app from that staged copy inside a `do { … } while` loop. `cargo build --release` then overwrites `target\release\` freely — the running file is the staged copy, not the build output. On each loop iteration the supervisor re-stages the (possibly freshly built) binary before launching.

The supervisor owns the SSH tunnel (as before) and keeps it alive *across* relaunches: only a real quit tears it down. The remote backend is persistent, so a relaunched frontend reconnects with its cached `(session_id, last_seen_revision)` and the daemon replays missed events — no state lost.

### 2. Sentinel exit code 75 = "rebuild done, relaunch me"

The frontend exits with code **75** to request a relaunch; any other code is a real quit. The supervisor reads `Process.ExitCode` after the poll loop and loops iff it is 75, passing `--relaunched` to the next launch. `std::process::exit(75)` is called directly from the frontend's window-event handler — abrupt teardown is acceptable since UI/session state is persisted on events and the OS reclaims the window/GPU surface.

### 3. File sentinel as the relaunch trigger (not a keybind alone)

The relaunch is triggered by the presence of `%LOCALAPPDATA%\devenv\relaunch.request` (`$XDG_STATE_HOME/devenv/relaunch.request` on Unix). A background watcher thread polls for it every 400 ms; on first sight it deletes the file, sets a flag, and wakes the window. The window-event handler then exits 75.

Rationale: the dogfooding driver is a program in the terminal (`claude`), which cannot press keys but can write a file. A file sentinel is the simplest cross-process signal that both a human and an in-terminal agent can raise. `scripts/relaunch-devenv.ps1` wraps the common case: `cargo build --release`, then drop the sentinel **only on a green build** (a failed build leaves the app untouched). A polling thread (not a control-flow timer) keeps the interactive `Wait` power profile intact.

### 4. Terminal resume command on `--relaunched`

When started with `--relaunched`, the frontend opens straight into the Terminal drawer and runs a configured resume command on the shell's first spawn — `[terminal] resume_command`, default `claude --dangerously-skip-permissions --continue /sot-fe-session-start` (a bare `claude --continue` resumes into permission-prompt mode and the session stalls — 2026-07-09). The command is injected as **shell launch args** (`-NoExit -Command` for PowerShell, `/K` for cmd, `-c "…; exec $SHELL"` for POSIX), not written to PTY stdin, to avoid the race where input arrives before the shell's first prompt. The shell's working directory is set to the repo root (`$SOT_REPO_DIR`, exported by the supervisor) so `claude --continue` resumes the right project's session.

## Consequences

- **Positive:** full hands-free loop — an in-terminal `claude` runs `relaunch-devenv.ps1`, the app rebuilds and respawns, and the resumed session reattaches itself. After one bootstrap onto the supervisor, the loop is self-sustaining and every subsequent rebuild works in place.
- **Positive:** no new architecture — reuses the persistent-backend reconnect, the existing tunnel supervisor, and the ADR 0016 local-child PTY. No reattachable PTY host.
- **Negative / accepted:** the terminal's scrollback and any non-`claude` process in the pane are lost on relaunch. This is the explicit trade for not building a persistent PTY host; `claude --continue` covers the one session that matters.
- **Negative:** the *first* migration onto the supervisor can't use the in-place loop (the old running frontend locks `target\release` and has no exit-75 path). It needs a one-time bootstrap: kill the old frontend, build, then start `launch-devenv.ps1 -Relaunched`.
- **Bootstrap escape hatch:** if auto-resume ever fails, recovery is manual and cheap — run `claude --continue` in the repo dir, or relaunch via the Desktop shortcut.

## Alternatives considered

- **Build-while-dead (no staging):** frontend exits 75, supervisor builds while the app is down, then relaunches. Rejected: ~45 s of blank downtime and, worse, build output/errors are invisible (no live pane). Staging lets the build run live with visible output and only relaunches on success.
- **Persistent/reattachable terminal (Windows mini-tmux):** would let arbitrary sessions survive. Rejected as unnecessary given `claude --continue`, and in tension with ADR 0016.
- **Keybind-only trigger:** insufficient — the in-terminal agent driving the loop can't synthesize keystrokes. A file sentinel serves both human and agent; a keybind can be layered on later as a convenience that drops the same sentinel.
