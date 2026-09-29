# Architecture Decision Records

This directory is a decision log. Each ADR records why a decision was made,
at the time it was made — not a description of how the system works today.
Nothing in the load path should send a reader here for current behavior;
read `CLAUDE.md` and `requirements.md` for that.

Every ADR's line 3 begins with one of three status tokens:

- `current` — the record is not superseded. This covers accepted, proposed
  and not-yet-implemented decisions alike; the implementation state stays in
  the prose, not in the token.
- `superseded by ADR NNNN` — the whole decision is retired; ADR NNNN replaced
  it.
- `partly superseded by ADR NNNN` — the decision stands; some machinery it
  prescribed was retired by ADR NNNN.

## Current ADRs

- [0001](0001-protocol.md) — Line protocol
- [0002](0002-kernel-launch.md) — Kernel launch and process supervision
- [0003](0003-rendering-surface.md) — Rendering surface (revised)
- [0004](0004-llm-cache.md) — LLM provider and prompt-cache layout
- [0005](0005-ast-hash.md) — AST hash algorithm
- [0006](0006-plugin-discovery.md) — Plugin discovery
- [0007](0007-tool-registration.md) — Tool registration timing
- [0008](0008-project-root.md) — Project root detection
- [0009](0009-repl-frames.md) — REPL streaming frame format
- [0011](0011-rendering-split.md) — Rendering split — chrome via ratatui, previews via parallel surface
- [0012](0012-frontend-stack.md) — Frontend rendering stack
- [0016](0016-frontend-local-terminal.md) — Frontend local terminal pane — PTY, emulator, render, and drawer-state choices
- [0018](0018-video-playback.md) — Video — poster in the pane, playback in the browser
- [0019](0019-frontend-control-channel.md) — Frontend control channel + state readback
- [0020](0020-server-monitoring.md) — Server monitoring — SSH-poll data plane, native drawer view
- [0021](0021-pdf-preview.md) — PDF preview — poppler rasterization behind a paged `preview.get`
- [0022](0022-capture-roi-to-llm.md) — Capture an image-preview ROI to the LLM pane
- [0027](0027-connection-reaper.md) — Half-open connection reaper — TCP keepalive + bounded write-timeout
- [0033](0033-session-repl-execute.md) — session-driven REPL execute + output collect (`repl.execute`)
- [0034](0034-dynamic-scalebar.md) — dynamic scalebar for the preview pane
- [0035](0035-daemon-tcp-proxy.md) — Daemon TCP proxy — any backend page through the one control tunnel
- [0039](0039-voyage-frame-codec-and-segment-format.md) — Voyage frame codec and segment format (Ship's Log P1, v1)
- [0040](0040-claude-producer-adapter.md) — The Claude producer adapter (Ship's Log P2)
- [0041](0041-fe-local-capsules-windows.md) — FE-local capsules on Windows (Ship's Log P3)
- [0042](0042-first-class-local-sessions.md) — First-class local sessions and the multi-host selector
- [0043](0043-l1-unix-capsule-runtime.md) — L1-unix — the capsule runtime on Unix hosts
- [0044](0044-turn-end-floor-done.md) — Blue / gray as unread / read — the turn-end floor writes `done`
- [0045](0045-lane-bridge-and-protocol-gate.md) — The lane bridge, and the protocol-versioned lane gate
- [0046](0046-declared-identity-and-daemon-owned-rows.md) — Declared identity, daemon-owned rows, and the resident attach
- [0049](0049-messaging-on-one-page.md) — messaging on one page

## Superseded records

- [0003](0003-terminal-images-superseded.md) — Terminal image protocol → ADR 0003 (`0003-rendering-surface.md`)
- [0010](0010-transport-and-persistence.md) — Transport, persistence, and reconnect → ADR 0046
- [0013](0013-backend-sessions.md) — Backend sessions — tmux registry, lifecycle, resume → ADR 0046
- [0014](0014-workspaces.md) — Workspaces — one daemon, direct-child kernels per workspace, routed by workspace_id → ADR 0046
- [0015](0015-hosts-targeting.md) — In-app host targeting via `hosts.toml` + `Mode::Hosts` → ADR 0042
- [0017](0017-frontend-self-relaunch.md) — Frontend self-relaunch — staged-copy supervisor, sentinel trigger, terminal resume → ADR 0041
- [0023](0023-daemon-fe-commands-and-spawn.md) — Daemon-brokered FE commands + daemon-boot session spawn → ADR 0025 and ADR 0046
- [0024](0024-backend-web-pages.md) — Open backend web pages in the local browser (dynamic port-forward) → ADR 0035
- [0025](0025-daemon-authoritative-fe.md) — Daemon-authoritative FE — imperative commands + FE-as-viewport → ADR 0046
- [0026](0026-rename-to-ship-of-tools.md) — Rename DevEnv.jl → "Ship of Tools" → ADR 0046
- [0028](0028-remote-comm-autoconnect.md) — Remote comm auto-connect — myhost-anchored reverse SSH tunnels under systemd --user → ADR 0046
- [0029](0029-multi-fe-docs-serving.md) — Multi-FE-correct `docs.open` site serving — per-connection site roots + disconnect cleanup → ADR 0035
- [0030](0030-versioning-release-and-auto-update.md) — Versioning, releases, and auto-update — going public → ADR 0046
- [0031](0031-codex-sessions.md) — Codex sessions (proposed) → ADR 0046
- [0032](0032-interactive-browser-figures.md) — Interactive browser-served figures (WGLMakie/Bonito) → ADR 0035
- [0036](0036-workspace-lifecycle-hygiene.md) — Workspace lifecycle hygiene — refuse duplicate roots, reap orphans → ADR 0046
- [0037](0037-ships-log-substrate.md) — The Ship's Log — sessions become durable records → ADR 0046
- [0038](0038-tmux-keeper-unit.md) — sot-tmux keeper — daemon restarts must stop killing sessions → ADR 0046
- [0047](0047-claude-ping-wake.md) — Claude sessions wake on a ping, not a harness Monitor → ADR 0049
- [0048](0048-filer-receipts.md) — the filer's receipt is the cross-box delivery verdict → ADR 0049

## Maintaining this invariant

Every file's line 3 must match the status-token pattern. Check it with:

```sh
ls docs/adr/0*.md | wc -l
for f in docs/adr/0*.md; do sed -n '3p' "$f" \
  | grep -qE '^\*\*Status:\*\* (current|(partly )?superseded by ADR [0-9]{4})' \
  || echo "BAD $f"; done
```
