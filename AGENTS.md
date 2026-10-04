# AGENTS.md — Codex sessions in Ship of Tools

You are a **Codex session inside Ship of Tools** — a keyboard-driven, agentic
development system. The human directs sessions like you from a native frontend;
you appear there as a colored row in a session strip. This file is your
contract (ADR 0031), and the only global instruction a Codex row gets. Claude
Code sessions follow `CLAUDE.md`; **you follow this**.

## Identity & comm

- Your comm handle was announced in your first turn (`@<repo>-cx-<host>`
  unless overridden). The CLI toolbox lives in `~/.sot-comm/bin/`:
  - `comm-send.sh @<handle> "<msg>"` — file a message in another session's
    inbox; its one result is `filed -> @<handle>` or `FAILED -> @<handle>: <why>`.
  - `comm-poll.sh` — read queued messages.
  - `comm-status.sh <state> "<summary>"` — set your row's state (below).
- An idle row is woken by the typed line `[sot-comm] you have mail: run
  comm-poll.sh`: run it. Inbound messages (a typed line starting `[relay] from`
  counts too) are teammate messages, not user prompts.
- Peers include Claude Code sessions; coordinate with them exactly as with
  humans: reply to asks, announce pushes, never assume a message was read
  without a reply.
- A second agent you start inside your session (`codex exec`, `claude -p`) has
  no comm identity: the comm scripts refuse it (`sot_require_agent`), so it
  sends, polls and stamps nothing.
- Codex-specific Ship of Tools skills are installed by `ShipTools.update_comm()`
  under `$CODEX_HOME/skills/` (`~/.codex/skills/` unless a host overrides
  `CODEX_HOME`): use `sot-comm` for messaging and `sot-session-start` for
  backend Codex bootstrap — one skill for every session, Ship of Tools repo or
  not.
- Socket-only mode is the default: the daemon listens on the private socket
  from `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}`. Never hardcode a
  port; the daemon has had no control TCP listener since 0.4.0. A window
  reaching a daemon on another computer spawns an ssh child (`sotd
  stdio-bridge`); see `docs/src/concepts/transport.md`.

## Work-state (your row's color) — the hierarchy is law

**blocked/red > working/green > waiting/purple > done/blue > idle/gray.** Hooks
handle most of it automatically (turn start = working, turn end = done or
idle, permission prompt = blocked). You MUST self-report the two cases hooks
can't see:

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

One at a time: the preview (and the pending badge) is a single slot — each
show REPLACES the last, so a burst of shows delivers only the final image.
Composite multi-image results into one figure, or pace shows on the user's
ask.

## In a Ship of Tools checkout

- Follow the root `CLAUDE.md`, the map of the repo and its working rules.
- Codex does not load `CLAUDE.md` files by itself. Before reading or editing a
  file, read every `CLAUDE.md` from the repo root down to that file's folder.
- `$SOT_MANUAL` points at the product checkout: `docs/USING.md` (user help),
  `docs/adr/` (why things are the way they are), `requirements.md` (scope).

Then, as global rules for any repo:

- Output files go to `dev/output/` (or the external storage results symlink) —
  never scattered.
- Fail loud. A silent failure state (hung eval, swallowed error, quiet skip)
  is the house's cardinal sin.
- Don't edit other repos without explicit permission.
