# Architecture Decision Records

This directory is a decision log. Each ADR records why a decision was made,
at the time it was made — not a description of how the system works today.
The root `CLAUDE.md` sends a reader here for four records — the window's
restart (ADR 0017), session spawn and daemon boot (ADR 0046), messaging
(ADR 0049), and the plugin discovery that is not built (ADR 0006). Read the
status token first: a record marked `partly superseded` can be right about
the thing you came for and wrong about the machinery around it.

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
- [0044](0044-turn-end-floor-done.md) — Blue / gray as unread / read — the turn-end floor writes `done`
- [0045](0045-lane-bridge-and-protocol-gate.md) — The lane bridge, and the protocol-versioned lane gate
- [0046](0046-declared-identity-and-daemon-owned-rows.md) — Declared identity, daemon-owned rows, and the resident attach
- [0049](0049-messaging-on-one-page.md) — messaging on one page
- [0050](0050-window-close-ends-this-computers-sessions.md) — the window close ends this computer's sessions

## Superseded records

- [0003](0003-terminal-images-superseded.md) — Terminal image protocol (superseded by ADR 0003 `0003-rendering-surface.md`)
- [0010](0010-transport-and-persistence.md) — Transport, persistence, and reconnect (partly superseded by ADR 0046)
- [0013](0013-backend-sessions.md) — Backend sessions — tmux registry, lifecycle, resume (superseded by ADR 0046)
- [0014](0014-workspaces.md) — Workspaces — one daemon, direct-child kernels per workspace, routed by workspace_id (partly superseded by ADR 0046)
- [0015](0015-hosts-targeting.md) — In-app host targeting via `hosts.toml` + `Mode::Hosts` (partly superseded by ADR 0042)
- [0017](0017-frontend-self-relaunch.md) — Frontend self-relaunch — staged-copy supervisor, sentinel trigger, terminal resume (partly superseded by ADR 0041)
- [0023](0023-daemon-fe-commands-and-spawn.md) — Daemon-brokered FE commands + daemon-boot session spawn (partly superseded by ADR 0025 and ADR 0046)
- [0024](0024-backend-web-pages.md) — Open backend web pages in the local browser (dynamic port-forward) (partly superseded by ADR 0035)
- [0025](0025-daemon-authoritative-fe.md) — Daemon-authoritative FE — imperative commands + FE-as-viewport (partly superseded by ADR 0046)
- [0026](0026-rename-to-ship-of-tools.md) — Rename DevEnv.jl → "Ship of Tools" (partly superseded by ADR 0046)
- [0028](0028-remote-comm-autoconnect.md) — Remote comm auto-connect — myhost-anchored reverse SSH tunnels under systemd --user (partly superseded by ADR 0046)
- [0029](0029-multi-fe-docs-serving.md) — Multi-FE-correct `docs.open` site serving — per-connection site roots + disconnect cleanup (partly superseded by ADR 0035)
- [0030](0030-versioning-release-and-auto-update.md) — Versioning, releases, and auto-update — going public (partly superseded by ADR 0045 and ADR 0046)
- [0031](0031-codex-sessions.md) — Codex sessions (proposed) (partly superseded by ADR 0046)
- [0032](0032-interactive-browser-figures.md) — Interactive browser-served figures (WGLMakie/Bonito) (partly superseded by ADR 0035)
- [0036](0036-workspace-lifecycle-hygiene.md) — Workspace lifecycle hygiene — refuse duplicate roots, reap orphans (partly superseded by ADR 0046)
- [0037](0037-ships-log-substrate.md) — The Ship's Log — sessions become durable records (partly superseded by ADR 0046)
- [0038](0038-tmux-keeper-unit.md) — sot-tmux keeper — daemon restarts must stop killing sessions (superseded by ADR 0046)
- [0041](0041-fe-local-capsules-windows.md) — FE-local capsules on Windows (Ship's Log P3) (partly superseded by ADR 0045)
- [0042](0042-first-class-local-sessions.md) — First-class local sessions and the multi-host selector (partly superseded by ADR 0045)
- [0043](0043-l1-unix-capsule-runtime.md) — L1-unix — the capsule runtime on Unix hosts (partly superseded by ADR 0045)
- [0047](0047-claude-ping-wake.md) — Claude sessions wake on a ping, not a harness Monitor (superseded by ADR 0049)
- [0048](0048-filer-receipts.md) — the filer's receipt is the cross-box delivery verdict (superseded by ADR 0049)

## Maintaining this invariant

**The two lists above are generated, not written.** They are a view of the
records' own status tokens and titles, so the way to change a list is to change
the record and regenerate. Both checks must pass; neither prints anything when
the directory is sound.

```sh
# 1. every record's line 3 carries one of the three tokens
for f in docs/adr/[0-9][0-9][0-9][0-9]-*.md; do sed -n '3p' "$f" \
  | grep -qE '^\*\*Status:\*\* (current|(partly )?superseded by ADR [0-9]{4})' \
  || echo "NO TOKEN $f"; done

# 2. the lists are exactly what the records say — regenerate and diff.
#    This is one comparison rather than several checks: it catches a record
#    missing from a list, a line for a record that no longer exists, a title
#    that has drifted from its heading, a pointer that disagrees with the
#    token, a duplicate, and a file under the wrong heading.
adr_lists() {
  for f in docs/adr/[0-9][0-9][0-9][0-9]-*.md; do
    b=${f##*/}; s3=$(sed -n '3p' "$f")
    tok=$(printf '%s' "$s3" | grep -oE '^\*\*Status:\*\* ((partly )?superseded by ADR [0-9]{4}( and ADR [0-9]{4})?|current)' | sed 's/^\*\*Status:\*\* //')
    named=$(printf '%s' "$s3" | grep -oE 'superseded by ADR [0-9]{4} \(`[^`]+`' | grep -oE '`[^`]+`')
    title=$(sed -nE '1s/^# ADR [0-9]+ *(:|—|-) *//p' "$f")
    case "$tok" in
      current) printf 'C\t- [%s](%s) — %s\n' "${b%%-*}" "$b" "$title" ;;
      *)       printf 'S\t- [%s](%s) — %s (%s%s)\n' "${b%%-*}" "$b" "$title" "$tok" "${named:+ $named}" ;;
    esac
  done
}
adr_listed() {
  awk '/^## Current ADRs/{t="C";next} /^## Superseded records/{t="S";next} \
       /^## Maintaining/{t=""} /^- \[/&&t{print t"\t"$0}' docs/adr/README.md
}
diff <(adr_lists | sort) <(adr_listed | sort)
```
