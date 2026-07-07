# ADR 0023: Daemon-brokered FE commands + daemon-boot session spawn

**Status:** Accepted (FE-arch sign-off 2026-06-18)
**Date:** 2026-06-18

> **Update — 2026-06-26: spawn unified on the wait-for-attach wrapper + daemon boot-pty (commits 65338b8, 26f9dc0, 41eab5d, bdce722).**
> **This Update is the CURRENT design. The Context/Decision below it is the original
> 2026-06-18 proposal, kept for history — read it as superseded.** For the FE command
> channel (§1–§2/§4 below) read **ADR 0025**, which owns it now.
>
> **One boot path.** The old model split boot into (a) a daemon background-pty boot
> that *typed* `ccb` into the pane for `boot=true` comm-spawns (§3) and (b) the FE
> *typing* `ccb` on attach (`autostart_claude_in_pane`) for nav-pane workspaces. Both
> typed into a shell and raced the prompt; maintaining two paths meant each fix landed
> on one while the other regressed — the split was the bug. Now `handle_workspace_create`
> gives **every** `autostart_claude` workspace (comm-spawn *and* nav-pane) a tmux pane
> **start command** of `boot_wrapper_command` (pty.rs): a wait-for-attach wrapper that
> blocks until a client attaches (`session_attached>0`), then `exec`s `ccb` (which **full-path**-`exec`s claude — never a bare `claude`; see the
> root cause below). claude becomes the pane process, so `pane_current_command` reports
> `claude`/`node` for detection — no typing, no prompt race.
>
> **The daemon boot-pty fires for every autostart create, not just `boot=true`**
> (handlers.rs gate `(req.autostart_claude || boot) && tmux_ok`, commit 41eab5d).
> `boot_workspace_claude` opens a real pty client (120×40) to the new session; that
> **stable** client satisfies the wrapper's wait, claude boots, the daemon polls
> `pane_current_command` until claude is foreground, then detaches (claude survives).
> This is what makes a **nav-pane** create boot reliably too: the ADR-0014 single
> foreground pty re-target is **not** a stable init client, so without the boot-pty a
> nav-pane claude died during init and the daemon bounced to home-base (the "sitting in
> home" bug).
>
> **Final root cause of the <1s boot death (commit bdce722).** `ccb`/`ccbe` ended with
> a **bare** `exec claude`. A daemon-spawned tmux pane inherits the **tmux server**
> environment (captured when the server started), **not** an interactive login shell —
> so its `PATH` lacks `~/.local/bin`, where `claude` lives → `claude: not found` → the
> exec fails → the pane dies in <1s → `boot_failed` → bash. Invisible because the
> boot-pty discards the pane's output. Fix: `ccb` and `ccbe` now **full-path** claude
> (`exec "$HOME/.local/bin/claude" …`) and `export PATH`. The boot-pty + wrapper + gate
> were all correct; this launcher-PATH bug was the last wall. **NB:** the Context below
> concludes "claude's TUI needs a real terminal at init — it exits in a detached pane."
> The <1s death was actually the PATH bug (`claude: not found`) **masking** the working
> boot-pty — not a terminal deficiency, and **not** evidence the boot-pty "can't boot
> claude" (it can, with its client). The boot-pty + wait-for-attach wrapper are
> **retained and load-bearing**: the wrapper blocks until a client attaches
> (`session_attached>0`) before exec'ing `ccb`, so the boot-pty is what fires the boot
> for a headless create. Do **not** read this as license to drop them.
>
> **Retired — do not reintroduce:** (1) typing `ccb\r` into the pane via the boot-pty's
> stdin (`boot_workspace_claude`'s old `writer.write_all`); (2) the FE
> autostart-on-attach **typing** (`pending_autostart` arming in `attach_session_to_bl`,
> removed 26f9dc0; the `autostart_claude_in_pane` scan/deliver code is inert, pending a
> cleanup); (3) gating the boot-pty to `boot=true` only. Task delivery is unchanged —
> comm-spawn owns it via comm. Launchers the daemon spawns into a tmux pane must
> **full-path their binaries** (or `export PATH`) — never rely on an interactive PATH.

## Context

Several workflows need a **backend session to drive the frontend deterministically** —
e.g. show an image in the preview pane, or **spawn a real FE session** (a workspace
visible and usable in the running frontend) from another session, with no user
action. Today this is attempted two ways, both wrong for the job:

1. **The sot-comm relay (LLM-routed).** A BE session sends a chat message to the
   FE's `claude` and hopes it acts. Non-deterministic, live-only, and routes through
   the FE LLM. `nav.preview` (the existing session→FE nav) rides this — a broadcast
   `agent.message` carrying a `{"sot_ui":…}` envelope that the FE *interceptor*
   parses before the LLM. Deterministic FE-side, but still on the comm transport.
2. **ADR-0019's frontend control channel.** This is the right *sink*: a watched
   directory of JSON command files → `FeCommand` → `dispatch_fe_command()`, driving
   the FE through the same methods the keybinds use (`switch_to_workspace`,
   `enter_mode`, nav `Action`s, …). LLM-free and comm-free. **But the file-dir is
   local to the FE's own machine** — written by the *co-located in-terminal agent*.
   A backend session on a remote host cannot write to a Windows FE's
   `%LOCALAPPDATA%\devenv\fe-commands\`. So ADR-0019 covers same-machine only.

Separately, **spawn "doesn't work unless you move to the session."** Root cause,
confirmed empirically this session: `claude`'s TUI needs a **real terminal at init**
— it exits cleanly in a detached tmux pane (even with a scrubbed env + a `script`-pty
client), but **survives once running** (long-running agents run detached right now).
The current design defers the boot to the FE's autostart-on-*attach*, so nothing
boots until a user navigates the drawer there.

## Decision

### 1. One sink, two transports — extend ADR-0019, don't duplicate

Keep `dispatch_fe_command(FeCommand)` (gpu.rs) as the single dispatch point and the
single command vocabulary. Add a **second transport** for remote/backend senders:

- **New daemon→FE op `op::FE_COMMAND`** carrying a versioned envelope
  `{v:1, cmd, args}`. The daemon broadcasts it to connected FE(s); the FE handles it
  in its **existing** `IncomingEvt::Event{op,payload}` match (same arm as
  `WORKSPACE_CHANGED` / `AGENT_MESSAGE`), one new branch that parses the envelope into
  an `FeCommand` and calls the same `dispatch_fe_command`. One op + versioned envelope,
  not a proliferation of ops — one dispatch point, room to grow.
- **BE→daemon send path:** a thin `devenv-fe <cmd> [args]` CLI frames the request and
  sends it to the daemon socket — the same `nc`-to-`tcp:127.0.0.1:PORT` path
  `comm-spawn.sh` already uses for `workspace.create`. No comm relay, no FE LLM.
- ADR-0019's local file-dir transport stays for the co-located in-terminal agent.
  Both feed the same sink.

This **supersedes** the `nav.preview`-over-`agent.message` piggyback: migrate it onto
`op::FE_COMMAND` so it is neither LLM- nor comm-routed.

### 2. Genuinely-new FE pieces: exactly two (everything else reuse)

win-fe verified the FE side: the `FeCommand` enum (gpu.rs:10454 —
`Workspace`/`Mode`/`Nav`/`Notify`/`CaptureRoi`), `dispatch_fe_command` (3523), and the
`fe-commands/` file-dir watcher all already exist. So the new FE work is only:

- **(a) the `op::FE_COMMAND` transport** — parse the envelope, push onto the existing
  `fe_commands` queue; `drain_fe_commands` / `dispatch_fe_command` handle it unchanged.
- **(b) two new `FeCommand` variants:**
  - `Preview { workspace, path }` — **reuse `handle_nav_envelope` verbatim**
    (gate → `Mode::Files` → cursor-reveal → `preview.get`), fed by the command instead
    of `agent.message` text.
  - `PreviewImage { workspace, path }` — **reuse `preview.get` + `png.rs`** (the preview
    pane already renders images; this is `preview.get` for an image node).

**No `Reveal` variant.** Focus/reveal is the **existing** `FeCommand::Workspace{slug}`
— it already switches the active workspace. Add a `Workspace{slug, attach_drawer:bool}`
flag only if a spawn should also attach the drawer; otherwise reveal is free. No new
boot or render code in `fe.*`.

### 3. Spawn = daemon-side boot, split into "visible" vs "focused"

`fe.spawn` = **`workspace.create` with `boot=true`** — NOT a parallel spawn op. Reuse
the create path the FE already surfaces via `workspace.changed`; the daemon boots
claude post-create.

- On a `boot=true` create the **daemon** does a **daemon-side background-pty boot**:
  open a server-internal pty to the new workspace's tmux pane, write the `ccb` launcher
  (with the teammate/`CLAUDECODE` env scrub), wait until `pane_current_command ==
  claude` (the `90c3f3b` guard signal = "claude is up"), then **close the boot pty**.
  claude survives the detach. Works with **zero FE attached** — this is the whole
  point: it kills the must-navigate dependency and the multi-FE race (a broadcast boot
  would have N FEs racing to launch `ccb`). The daemon owns tmux + ptys, so this is the
  correct layer (CLAUDE.md: daemon supervises processes). **`fe.*` must not reimplement
  boot.**
- **Boot timeout is bounded** (~30–60s, sized to cover `ccb`'s `/sot-session-start`
  first turn). On timeout the daemon surfaces a `boot_failed` workspace state and does
  **NOT** infinitely retry (mirrors the FE's `AUTOSTART_LAUNCH_HOLD` reasoning).
- **Visible = automatic, free:** `workspace.create` → `workspace.changed` → the FE's
  `workspace.list` poll adds the session to the Sessions list + bottom strip. No nav,
  no command.
- **Focused = explicit, opt-in:** the existing `FeCommand::Workspace{slug}` focuses the
  workspace (+ an optional `attach_drawer` flag). Never automatic — see §4.

### 4. Multi-FE targeting (mandatory on every fe.* command)

The daemon broadcasts to **all** connected FEs (`to` is advisory). Every command needs
a "which FE(s) act" rule, or one command fires on every roaming FE at once:

- **preview / preview_image:** GATE on **active-workspace** (nav.preview's proven
  model) — only the FE currently viewing that workspace acts. Put the gate **inside the
  `Preview`/`PreviewImage` dispatch arm** (in `dispatch_fe_command`), NOT at the
  transport layer — so **both** transports (file-dir + `op::FE_COMMAND`) inherit it for
  free.
- **focus (`Workspace{slug}`):** an **explicit, targeted** switch — never broadcast-act,
  so a spawn doesn't yank every roaming FE's focus.

### 5. Compose with the foreground guard (`90c3f3b`)

The `pane_current_command`-based foreground guard is now the **authoritative,
FE-relaunch-survivable** launch coordinator. Daemon boots claude detached → if a user
later navigates in, the FE's guard sees `claude` foreground and **skips** the FE `ccb`
launch (no double-launch). `autostart_claude_in_pane` remains only for **FE-created**
workspaces the daemon didn't boot; the guard prevents double-launch in both
directions. The FE's in-memory `launching_sessions` / `autostarted_sessions` become a
secondary optimization — they don't know about a daemon-boot and don't need to.

## Consequences

- Backend sessions (local **or remote**) drive the FE deterministically over the
  daemon — no LLM, no comm relay.
- Spawn no longer requires a user navigation: daemon boots, the session is visible
  automatically, focus is an optional explicit `reveal`.
- `nav.preview` migrates off the comm relay onto `op::FE_COMMAND`.
- One sink (`dispatch_fe_command`) + one vocabulary, two transports (ADR-0019 local
  file-dir for the in-terminal agent; `op::FE_COMMAND` for remote/backend senders).
- The boot pty is daemon-owned; the foreground guard is the single source of "is
  claude up."

## Open / deferred

- **`devenv-fe` CLI** (the BE-side sender): thin `nc` wrapper, mirrors `comm-spawn.sh`'s
  endpoint resolution. Mechanism decided; just needs writing.
- **foreground-guard runtime verify** (attach+roam, still pending) —
  parallel, not blocked by this ADR.

*Resolved at sign-off (win-fe, 2026-06-18):* `fe.spawn` = `workspace.create boot=true`
(not a parallel op); boot-pty timeout bounded ~30–60s + `boot_failed` state, no infinite
retry; no `Reveal` variant — reuse `Workspace{slug}`; active-workspace gate lives in the
`Preview` dispatch arm so both transports inherit it.
