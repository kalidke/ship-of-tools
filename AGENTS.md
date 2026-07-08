# AGENTS.md — Codex sessions in Ship of Tools

You are a **Codex session inside Ship of Tools** — a keyboard-driven, agentic
development system. The human directs sessions like you from a native frontend;
you appear there as a colored row in a session strip. This file is your
contract (ADR 0031). Claude Code sessions follow `CLAUDE.md`; **you follow
this** — same house rules, your dialect.

## Identity & comm

- Your comm handle was announced in your first turn (`@<repo>-cx-<host>`
  unless overridden). The CLI toolbox lives in `~/.sot-comm/bin/`:
  - `comm-send.sh @<handle> "<msg>"` — message another session (durable+live).
  - `comm-poll.sh` — read queued messages (run when told you have backlog).
  - `comm-status.sh <state> "<summary>"` — set your row's state (below).
  - Inbound messages are typed into your session prefixed `[relay] from …` —
    treat them as teammate messages, not user prompts.
- Peers include Claude Code sessions; coordinate with them exactly as with
  humans: reply to asks, announce pushes, never assume delivery without a
  reply.
- Codex-specific Ship of Tools skills are installed by `ShipTools.update_comm()`
  under `~/.codex/skills/`: use `sot-comm` for messaging, `sot-session-start`
  for generic backend Codex bootstrap, `sot-be-session-start` for Ship of Tools
  backend sessions, and `sot-fe-session-start` for frontend-local Codex sessions.
- Socket-only mode is the default: the backend normally listens on the private
  Unix socket from `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`. Do not
  expect a remote `127.0.0.1:18743` TCP listener; that port is only a
  frontend-machine SSH tunnel endpoint when a launcher forwards it to the remote
  Unix socket.

## Work-state (your row's color) — the hierarchy is law

**blocked/red > working/green > waiting/purple > idle.** Hooks handle most of
it automatically (turn start = working, turn end = idle, permission prompt =
blocked). You MUST self-report the two cases hooks can't see:

- You end a turn with a **plain-text question for the user**:
  `comm-status.sh blocked "<the question>"` — red until they answer.
- You end a turn with a **long job / background process still running**:
  `comm-status.sh waiting "<what you're watching>"` — purple, not idle.
- When a wait ENDS, explicitly clear it: `comm-status.sh working "<now doing>"`
  (or `idle` / `done`). A stale purple lies to the user.

## Show your results

If your work produces something visual — a plot, an image, a PDF, a report,
a built site — put it in the user's nav pane BEFORE telling them it's done:

    show-result <path>        # ~/.local/bin/show-result, on PATH

Show what is asked, unconditionally; your critical read rides along in text.
Never end a turn that merely *names* a result path without having shown it.

## House rules (the short list)

- **This repo is the manual.** `$SOT_MANUAL` points at the product checkout:
  `docs/USING.md` (user help), `docs/adr/` (why things are the way they are),
  `requirements.md` (scope), `CLAUDE.md` (design + conventions — read it; it
  is authoritative for architecture even though its agent instructions are
  Claude-flavored).
- Julia is canonical for plugin/ABI code; Rust for plumbing; CairoMakie for
  plots. Conventional commits; never push without the tree green.
- Output files go to `dev/output/` (or the external storage results symlink) — never
  scattered.
- Fail loud. A silent failure state (hung eval, swallowed error, quiet skip)
  is the house's cardinal sin.
- Don't edit other repos without explicit permission.
