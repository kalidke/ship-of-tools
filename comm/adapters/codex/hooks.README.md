# codex state-nav hooks — why `hooks.json` looks like that

Companion to `hooks.json` in this directory (ADR 0031). The documentation lives
here and **not** inside the JSON, for a reason given below that cost the project
weeks of silently-missing session colours.

`hooks.json` wires codex lifecycle events to the shared `comm-status-*.sh`
scripts, so a codex session reports its work-state into the state-nav exactly
like a Claude Code session does:

| event               | script                     | state            |
|---------------------|----------------------------|------------------|
| `UserPromptSubmit`  | `comm-status-working.sh`   | working / green  |
| `PostToolUse`       | `comm-status-heartbeat.sh` | working / green  |
| `Stop`              | `comm-status-idle.sh`      | idle (neutral)   |
| `PermissionRequest` | `codex-status-blocked.sh`  | blocked / red    |

`PermissionRequest` maps to blocked because codex has no `AskUserQuestion` tool;
a permission prompt is its nearest "needs the USER to act", which is what red
means. Its payload carries a top-level `tool_name`, which is why
`codex-status-blocked.sh` can name the asking tool in the summary. Because these
are the same bin scripts the Claude adapter uses (the payloads share the field
names that matter — notably `prompt`), the state HIERARCHY (blocked/red >
working/green > waiting/purple > idle, 2360fca) is enforced identically: the
working hook's machine-turn guard, the heartbeat's waiting→working promotion,
soft idle's sticky demote. Every script self-gates on the pane's registry row, so
codex sessions **outside** Ship of Tools are silent no-ops.

`ShipTools.update_comm()` deploys the file into the sot-comm plugin under
`$CODEX_HOME` (`codex plugin add` re-copies it on every run, so a repo edit does
propagate — verified).

## The two rules this file must obey

Measured against codex-cli **0.146.0** using codex's own `/hooks` TUI panel,
which reports an `Installed` and `Active` count per event, and corroborated
against the 0.142.4/0.142.5 binaries and the `codex-rs` source.

**1. Only `"hooks"` may appear at the TOP level.** The hooks config struct is
`deny_unknown_fields`, so a single unrecognized top-level key makes codex reject
the **entire file** — every event drops to `Installed = 0`, with no error printed
anywhere: not at startup, not in a log, not to the session. This file used to
carry a `"_comment"` key holding the notes now in this README, and that one key
was enough to disable state reporting for the whole codex fleet. Verified by
deleting only that key from an otherwise byte-identical file (`Installed` 0 → 1
on all four events) and reproduced with a differently-named key (`_note`).
**Do not put prose back into `hooks.json` — it belongs here.** `install_comm`
now fails loudly if a stray top-level key reappears.

Note the asymmetry: unknown keys *inside* the `hooks` object are ignored
harmlessly; only the top level is fatal.

**2. Event keys are PascalCase.** `UserPromptSubmit`, `PostToolUse`, `Stop`,
`PermissionRequest`. The Rust config uses `serde(rename = "PermissionRequest")`
with **no alias**, in 0.142.5 and 0.146 alike, so a snake_case key is simply an
unrecognized inner key and is dropped in silence.

### The misdiagnosis worth remembering

An earlier revision of this file used snake_case keys, under a comment asserting
that snake_case was "verified against the codex 0.142 binary" and that PascalCase
was "silently ignored — the original silent-no-color bug". **That was wrong, and
it inverted the truth for weeks.** Running the actual 0.142.4 and 0.142.5
binaries shows they accept PascalCase only; no released version has ever accepted
snake_case. The likely story: hooks were not firing because of the fatal
`_comment` key (rule 1), the investigation reached for the event-key case
instead, "confirmed" it by finding snake_case strings in the binary — those are
codex's *internal canonical* names, which is also what the `[hooks.state]` trust
keys use — and changed the file to a spelling that never worked. The original
defect was untouched, and the file now had two.

The lesson is not "snake_case bad". It is that **a strings-grep is not a test**,
and that both failure modes here are invisible: nothing anywhere reports a
rejected file or a dropped key. The `/hooks` panel is the only instrument that
tells the truth, and checking it takes five seconds.

A dual-spelling file registering both cases was considered and rejected: it buys
nothing today (snake_case is dropped either way), and if snake_case were ever
added as a serde alias, having both keys would be a **fatal duplicate-field
parse error** — turning a hypothetical future compatibility win into the exact
total-silent-failure this file keeps falling into.

## Hook trust

A newly-defined hook is untrusted until reviewed: `/hooks`, then press `t`. Trust
is persisted in `$CODEX_HOME/config.toml` under `[hooks.state]`, keyed per hook by
a `trusted_hash` of its **definition** — the command and timeout, *not* the event
key spelling. (Measured: freshly trusting the corrected PascalCase file produced
`trusted_hash` values byte-identical to those recorded when the file still used
snake_case keys, and the trust keys themselves are snake_case in both cases. So
correcting the key case does **not** invalidate existing trust; changing a hook's
command does.)

`ccx` passes `--dangerously-bypass-hook-trust`, and per the codex hook-discovery
implementation a hook runs when `bypass_hook_trust || trusted` — so that flag
genuinely does execute an untrusted hook. A `/hooks` panel showing `Active = 0`
reflects *persisted* trust and does not predict what happens under the flag.
Trusting once per machine is still worth doing, because sessions started without
`ccx` get no bypass.

`$CODEX_HOME` may be **machine-local** (this deployment points it at
`/var/tmp/$USER/codex` to keep codex's `flock()` off NFS), in which case trust,
plugin enablement and the plugin cache are per-machine and a shared `$HOME` does
not cover them — `update_comm` has to run on each machine.

`[features] hooks = true` was needed by an older codex; on 0.146 hooks install
without it, so its absence from a config is not a fault.

## What this does NOT fix

`ccx` launches with `--dangerously-bypass-approvals-and-sandbox`, which skips all
confirmation prompts — so a ccx-launched session never emits a
`PermissionRequest` at all and will never go red from this hook. That is correct
(such a session genuinely never blocks on permission), but it means red appears
only for plain or resumed codex sessions. Do not read its absence under `ccx` as
this wiring being broken again.

## Re-check after any codex upgrade

Both failure modes above are silent. After upgrading codex, open `/hooks` and
confirm the four events read `Installed = 1` and `Active = 1`. The repo's test
suite checks the file's shape, but only the running binary can tell you whether
*this* codex still accepts it.
