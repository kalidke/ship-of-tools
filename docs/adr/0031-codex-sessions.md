# ADR 0031 — Codex sessions (proposed)

**Status: ACCEPTED + P1 IMPLEMENTED** (2026-07-04). The maintainer's decisions:
ChatGPT-login auth (no API-key plumbing) · `-cx-` handles · AGENTS.md authored
by the installer lane · folder-only = tmux+shell at the workspace root.

**P1 shipped and live-tested on the dev backend**: `workspace.create agent:"codex"` →
daemon boots a codex pane via `ccx` (nvm full-path resolution, env scrub,
comm join + listener + `codex-watch` pane-injection wake, per-dir pre-trust,
`--sandbox danger-full-access` — the ccb-equivalent posture; codex's default
sandbox makes $HOME read-only, which broke every comm CLI). Proven loop: a
message sent to the codex session was injected into its pane, processed, and
answered over comm ("CODEX-WAKE-OK") — cross-agent messaging works. Known P2
gaps: ~/.codex/hooks.json state hooks did not observably fire in the live
test (verify codex hook loading/trust gating; until then codex rows rely on
explicit comm-status calls per AGENTS.md), FE row sigils, comm-spawn
--agent codex.

## Context

Decision: implement Codex (OpenAI Codex CLI) sessions alongside Claude Code
sessions. Create-dialog UX: **Enter → Claude Code · Shift+Enter → folder only
(no LLM) · Ctrl+Enter → Codex**. Codex has its own memory and skill systems,
different from CC's.

Codex CLI as of mid-2026 is far more integrable than at our project start: a
**stable lifecycle-hooks engine** (SessionStart, UserPromptSubmit, PreToolUse /
PostToolUse, PermissionRequest, Stop — nearly isomorphic to Claude Code's),
discovered from `hooks.json` / `[hooks]` tables beside active config layers; a
**skills system** with per-skill enable/disable; **AGENTS.md** project docs
read natively at session start; per-project `.codex/config.toml` overrides.
Our comm layer's CLI surface (`comm-send.sh`, `comm-status.sh`, `show-result`,
`sot-fe`) is plain bash — already agent-agnostic. What is CC-specific today:
the hook *wiring* (settings.json), the skills content, CLAUDE.md, and the
harness **Monitor** wake (codex has no equivalent primitive).

## Decision (proposed)

### 1. Spawn UX — three-key create
- Create dialog: **Enter** = Claude Code (today's autostart path),
  **Shift+Enter** = folder-only workspace (tmux + shell in the Terminal
  drawer, no agent), **Ctrl+Enter** = Codex session.
- Protocol: `workspace.create` gains `agent: "claude" | "codex" | "none"`;
  `autostart_claude: true` remains accepted as an alias for `agent: claude`.
- Persisted in the workspace toml; returned by `workspace.list`; the FE
  renders a small per-agent **sigil** on the session row.

### 2. Backend
- The ADR-0023 wait-for-attach boot wrapper branches on `agent`:
  exec `ccb` (claude) / `ccx` (codex, new) / plain shell (none).
- Env stamping (`SOT_SESSION`, `SOT_WORKSPACE`, `SOT_WORKSPACE_ROOT`,
  `SOT_MANUAL`) is agent-agnostic — unchanged.

### 3. `comm/adapters/codex/` — mirror of the claude adapter
- **`ccx` launcher**: wait-for-attach-compatible; execs `codex` so the
  SessionStart hook (below) performs the comm bootstrap (join as the session
  handle, start `comm-listen`, start `codex-watch`).
- **Hooks** (`hooks.json`, deployed beside `~/.codex/config.toml` by
  `update_comm()`): UserPromptSubmit → `comm-status working` **with the same
  machine-turn/blocked hierarchy guard** (red > green > purple, 2360fca);
  Stop → soft idle (sticky demote applies); PostToolUse → heartbeat
  (waiting→working promotion applies); **PermissionRequest → blocked** (codex
  has no AskUserQuestion; a permission prompt is the nearest "needs the
  user"). Identical bin scripts — zero new state logic.
- **Memory**: a committed **`AGENTS.md`** — codex-facing counterpart of
  CLAUDE.md: identity + handle, comm CLI usage, show-result discipline
  ("show what is asked"), state hierarchy + explicit waiting/blocked calls,
  `$SOT_MANUAL` as the manual root. Codex reads it natively (truncation knob:
  `project_doc_max_bytes` — keep it lean, link into the checkout).
- **Skills**: phase 1 inlines the essentials in AGENTS.md; phase 3 ships
  proper codex skills mirroring sot-comm / show-result.
- **Receive path**: `comm-listen.sh` files the inbox unchanged. The **wake**
  is the one CC-specific piece (no harness Monitor in codex): a small
  **`codex-watch`** per-session daemon tails the inbox (poll — NFS!) and
  injects inbound frames into the codex pane via `tmux send-keys`
  ("[relay] from X: …" + Enter). The pattern we retired for CC, revived
  where it is the right tool. Step 0 re-verifies whether codex has grown a
  background-wake primitive that supersedes this.
- **Turn auditor**: codex transcripts live in `~/.codex/sessions/` in a
  different format — phase 3 adapts the reader; until then the pane-idle
  detector + hooks are the truth floor.

### 4. Identity + interop
- Handle convention **`<repo>-cx-<host>`** so a CC session and a Codex
  session coexist on one repo without collision; registry row gains
  `agent: "codex"`.
- Cross-agent messaging works day 1 (both speak the comm CLIs).
- `comm-spawn.sh --agent codex` lets Claude sessions stand up Codex peers —
  the README's "Codex-driven agent sessions that interact with your Claude
  agents", literally.

### 5. Phasing
- **P1 (usable)**: protocol field + FE three-key create + BE launcher branch
  + `ccx` + AGENTS.md + working/idle hooks + `codex-watch` pane-inject wake.
  A Codex session spawns, shows correct green/idle, sends/receives comm.
- **P2 (parity)**: heartbeat + hierarchy guard verified under codex payloads,
  PermissionRequest→blocked, FE sigils, `comm-spawn --agent`.
- **P3**: auditor adaptation, codex skills, cross-agent review workflows,
  worktree flow for codex sessions.
- **Step 0 at implementation start**: re-verify hook event names/payload
  shapes + skills dir format against current codex docs (they ship weekly).

## 6. Decisions needed (maintainer)

1. **Auth**: ChatGPT login or API key on the fleet — which account, which
   boxes?
2. **Handle**: `<repo>-cx-<host>` OK?
3. **AGENTS.md author**: me now, or docs lane as part of repo-as-manual?
4. **Folder-only** (Shift+Enter): tmux + shell, no agent — confirm intent.

## References

- Codex hooks: https://developers.openai.com/codex/hooks
- Config reference: https://developers.openai.com/codex/config-reference
- CLI reference: https://developers.openai.com/codex/cli/reference
- Hooks guide (events/payloads): https://codex.danielvaughan.com/2026/04/15/codex-cli-hooks-complete-guide-events-policy-patterns/
- ADR 0023 (spawn wrapper), ADR 0025 (FE command channel), 2360fca (state
  hierarchy), ADR 0030 amendment (repo-as-manual / `SOT_MANUAL`).
