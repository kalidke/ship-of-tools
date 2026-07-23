---
name: sot-session-start
description: Bootstrap a (re)started backend Claude session so it RECEIVES instant fast-comm on the sot-comm network — start the durable relay listener, arm the inbox Monitor that wakes the session, prove the wake path, then catch up on anything missed while it was down. Generic across all projects (no app-specific steps). Runnable manually or as a `claude --continue` resume turn. Activates for "comm session start", "comm bootstrap", "rearm comm", "start relay listener", "receive setup".
---

# sot-session-start

The first turn after a backend (tmux) Claude session is (re)started or resumed
with `claude --continue`. A resumed session is **reactive and deaf**: harness
Monitors do NOT survive a restart (`--continue` restores the transcript, not live
background tasks), and the cross-machine relay is **live-only** (no server-side
queue). So until this turn re-establishes receiving, nothing wakes the session on
inbound fast-comm. This skill is that turn.

This is the **generic** sot-comm receive-bootstrap — useful for **any** backend
session on the network, regardless of project. Ship of Tools backend sessions instead run
`/sot-be-session-start`, which does everything here and then adds sot-specific
checks (frontend reachability, FE count, the `.claude-bus` git fallback). The `ccb`
launcher runs this skill; `ccbe` runs the Ship of Tools one.

## Steps

**Step 0 decides whether the rest runs at all.** If you SURVIVED a compaction you
are still fully connected — skip everything below. Otherwise (a cold start or a
`claude --continue` restart) you are deaf: do the three bootstrap steps — **(a)**
join (also your identity); **(b)** in parallel, start the listener + arm the inbox
Monitor + catch up on the down-window; **(c)** one post-arm selftest proves the
chain wakes you.

### (0) First: did you SURVIVE a compaction, or genuinely (re)start?

This skill is the **deaf-restart** bootstrap. A cold start or a `--continue`
restart genuinely kills your harness Monitor, so the full (re)join / listen /
arm / catch-up below is exactly right. But a **context compaction does NOT make
you deaf** — your Monitor and listener are background tasks that *survive* it.
Running the full bootstrap on a merely-compacted session is actively harmful: it
**double-arms** the Monitor (duplicate wakes, compounding per compaction),
**replays** already-handled messages via `comm-poll`, and **wipes your live
work-state** via `comm-join`'s row-replace. So branch first.

Get your handle and check for a **live watcher**:

```bash
eval "$(~/.sot-comm/bin/comm-context.sh 2>/dev/null)" 2>/dev/null || true   # sets NAME (empty when not joined) — eval, do NOT sed-scrape: values are %q-quoted, so a scrape can capture literal quotes as a bogus non-empty handle
h="${NAME:-$(basename "$PWD")-$(hostname -s)}"
h_re="$(printf '%s' "$h" | sed 's/\./\\./g')"   # escape dots — repo names contain them (e.g. LidkeLab.github.io-kitt); an unescaped '.' matches ANY char and could false-match a sibling
pgrep -u "$(id -un)" -f "comm-watch\.sh ${h_re}\$"   # dot-escaped + END-ANCHORED: neither a '.' nor a `-2` sibling can false-match (a false match would make a genuinely-deaf cold session skip arming → deaf)
```

> `comm-context.sh` validates the identity it returns: the self-file is keyed
> by tmux PANE ID, which tmux **reuses** after a server restart, so a fresh
> session in a recycled pane could otherwise inherit a *different* session's
> handle — making this very check pgrep the wrong watcher and conclude
> "survived" on a genuinely deaf cold start (observed 2026-07-23). A v2
> self-file records the repo the identity was claimed for; on mismatch the
> stale name is discarded and `NAME` comes back empty → the canonical
> fallback + full bootstrap run, which is correct for that case.

- **Prints a PID → you SURVIVED a compaction.** You are still connected —
  **STOP: do NOT run steps (a)–(c).** Re-reading this doc (and the `/sot-comm`
  skill) has already restored your operating context — handle, the send/poll/
  status verbs, the work-state rules — which is the whole point of re-running on
  compaction. You keep receiving on the watcher that never died; re-arming,
  re-polling, or re-joining would only harm. (If you *specifically* suspect the
  listener bridge died, `comm-listen.sh` is idempotent — running it is a safe
  no-op when the bridge is already up.)
- **Empty → you genuinely (re)started and are DEAF** (no watcher survived) →
  proceed with (a)–(c); the full bootstrap is correct.

> The `$`-anchor matters: `pgrep -f` is a substring match, so an un-anchored
> `comm-watch.sh repo-host` would also match a *sibling* session's
> `comm-watch.sh repo-host-2` and make a genuinely-deaf cold session wrongly
> skip arming — the exact deafness this skill exists to prevent.

### (a) Join — `comm-join.sh` (this IS your identity)

```bash
~/.sot-comm/bin/comm-join.sh        # no args: joins as the canonical default <repo>-<host>
```

`comm-join.sh` with **no args** joins as the canonical handle `<repo>-<host>`
(mixed-case preserved) and prints **`Joined sot-comm as @<handle>`** plus your
inbox path. That printed line **is** your identity — there is no separate
"resolve identity / re-check if empty" step. Use a non-default handle only if you
have a reason: `comm-join.sh --name <handle>` (or `--name=<handle>`). A rejoin
keeps an already-joined identity. Note the `@<handle>` — every command below uses
it as `<handle>`.

- If `~/.sot-comm/bin` is **absent**, install it first: from a Ship of Tools checkout
  run `julia --project=. -e 'using ShipTools; ShipTools.update_comm()'`, then join.

### (b) Listener + Monitor + catch-up — in PARALLEL

These three are independent; fire them together (one assistant turn, parallel
tool calls), then read the results.

1. **Start the durable relay listener.** It holds a connection to the relay daemon
   and files inbound messages into your inbox. The bridge **self-heals**: if the
   daemon closes the connection, the reconnect loop re-establishes it within ~2s
   (the hold uses bash `/dev/tcp`, whose read EOF fires on a graceful close — the
   old `nc` hold lingered in CLOSE-WAIT and froze the inbox until a manual restart).

   ```bash
   ~/.sot-comm/bin/comm-listen.sh        # start the bridge (no delivery proof here —
                                            # the post-arm selftest in (c) proves delivery)
   ```

   (`--status` shows the listener pid. The delivery proof is deferred to step (c)
   so it runs *after* the Monitor is armed and proves the wake, not just the file
   write — one selftest instead of two.)

2. **Arm the fast-comm wake** — a persistent harness **Monitor** whose command is
   `comm-watch.sh <handle>` (substitute the handle from step (a)):

   ```
   ~/.sot-comm/bin/comm-watch.sh <handle>
   ```

   (You only reach this step when Step 0 found **no** live watcher — a genuinely
   deaf cold start / restart — so arming here can't double-arm.) Use the
   **Monitor** tool with `persistent: true`, running exactly that command.
   `comm-watch.sh` is a poll loop (re-opens the inbox every 2s) that emits one line
   per new **directed** relay frame. **Poll — do NOT use `tail -F`.** The inbox is
   on **NFS** (`$HOME` is NFS on the Linux cohort) and `tail -F` relies on
   **inotify, which is unreliable over NFS** — it silently misses/delays writes
   (observed: a relay message surfaced **45 minutes** late). `comm-listen.sh`
   (step 1) only *files* the message; only this Monitor turns each new inbox line
   into an event that resumes the session. Both halves are required; this is the
   half a script can't do for you.

   **What wakes you vs. what only files** (the `comm-watch.sh` select): your own
   echoes never surface; **broadcasts** (`to:""`) **file silently** — both relay
   cc/announce traffic (the bridge stamps `to:""`) and durable
   `comm-send --broadcast` copies (stamped since 2026-06-12; before that a
   broadcast line had no `to` key, read as directed, and woke the entire network
   at once) — and are picked up by `comm-poll.sh` on your next natural turn.
   Everything else wakes you: direct relay frames (`to:` you, bridge to-preserving
   upgrade), durable directed `comm-send` lines (`to:` you, same 2026-06-12 stamp),
   and legacy pre-stamp lines (no `to` key at all). Wake-ups cost a model turn
   each — broadcasts are deliberately demoted.

3. **Catch the down-window gap** — read durable inbox messages queued while you
   were down (and advance the cursor):

   ```bash
   ~/.sot-comm/bin/comm-poll.sh
   ```

   Surface anything new. Unlike the live relay, the file inbox IS durable, so
   messages sent while you were deaf are still here. (`comm-poll.sh` filters out
   `__selftest__` frames, so the selftest in (c) won't show up here as a phantom
   "missed message" — and it no longer needs a tail position, so it's safe to run
   in parallel with the listener start.)

### (c) Prove the wake path end-to-end — one post-arm selftest

Arming the Monitor proves nothing until a real message actually *wakes* it. Run
**one** selftest now — *after* (b), so it proves listener + file-delivery + Monitor
wake in a single shot (this replaces the old two-selftest dance). (You only reach
this on a genuine cold start / restart — a survived-compaction session stopped at
Step 0 and never armed, so there is nothing to selftest.)

```bash
~/.sot-comm/bin/comm-listen.sh --selftest   # injects a from:__selftest__ to:<you> frame:
                                               # directed (passes the echo filter and the
                                               # broadcast demotion) so the Monitor MUST fire
```

- Inline, expect `selftest @<handle>: receive path OK` (or `RECOVERED after
  restart`). Exit codes: **0** OK; **3** = daemon reachable but bridge still
  connecting (cold start — *benign*, re-run `comm-listen.sh --selftest` in a few
  seconds; this is NOT a daemon problem); **1** = daemon endpoint missing or
  unreachable. In socket-only mode the scripts auto-discover the backend by
  querying `sotd session-socket-path ${SOT_BACKEND_LABEL:-sot}` and connecting
  to that Unix socket. Override only when needed:
  `SOT_RELAY_ENDPOINT=unix:/path/to/sot.sock` on the backend host, or
  `SOT_RELAY_ENDPOINT=tcp:127.0.0.1:<local-forward-port>` on a frontend machine
  whose local port forwards to the remote Unix socket. Do **not** expect a
  remote `127.0.0.1:18743` listener on socket-only backends.
- The real proof is the **Monitor notification** `[relay] from __selftest__: …`
  within ~2s — *that event*, not the inline `receive path OK`, confirms your
  session will wake on inbound.
- (To also confirm a specific **peer** is reachable cross-machine,
  `comm-relay.sh ask @<peer> "ping" 45` and require a reply: the daemon broadcasts,
  so any reply proves the round-trip; a 124 timeout is *not* proof of a dead path —
  the armed Monitor still catches a late reply.)

## Signal your work-state (the two cases the hooks miss)

Your nav-colour work-state is mostly automatic (Claude Code hooks:
`UserPromptSubmit`→working, `Stop`→idle, `AskUserQuestion`→**blocked**/red). **Two**
states are self-reported, and a freshly-booted session — exactly here — is where
they get missed:

- **A plain-text question to the user** (no AskUserQuestion tool) fires NO signal,
  so your row reads idle while you wait. Self-report first:
  `~/.sot-comm/bin/comm-status.sh blocked "<the question>"`.
- **You ended a turn with a long job / spawned subagent still running** — idle of
  your *own* work but NOT free:
  `~/.sot-comm/bin/comm-status.sh waiting "<what you're watching>"` → **purple**,
  not idle-green. A peer (or subagent) working in the background does NOT make you
  idle. Waiting is **sticky** (2026-07-02): set once, it survives later turn
  cycles (hooks demote you back to purple at each turn end) — clear it with an
  explicit `working`/`idle`/`done` when the job lands; it self-heals after 2h.

Precedence when more than one applies: **blocked** (needs the user) **>**
**waiting** (watching a job) **>** **idle** (free). Full treatment (auto-vs-manual,
soft-idle protection, sticky-waiting, clearing) is in the **sot-comm** skill's
"Work-state in the state-nav" section.

## Starting peer sessions (you can do this too)

A session can stand up new sessions on the network. Two flavors — pick by lifetime:

- **Durable comm-aware backend** — a long-lived peer that bootstraps its own
  receive path on start and on every `--continue` resume:

  ```bash
  tmux new-session -s <tmux-name> -c <repo-path> ~/.local/bin/ccb   # no -d: create AND attach
  ```

  `ccb` is this skill's launcher: it runs `claude` with `/sot-session-start` as
  the first turn, so the new session joins, listens, and arms its own inbox
  Monitor with no further help. For a Ship of Tools backend use `ccbe` instead (runs
  `/sot-be-session-start`). **Name the tmux session and the handle after the
  REPO, never the task** (canonical table: `comm/PROTOCOL.md` § Naming): handle
  defaults to `<repo>-<host>`, and the `<tmux-name>` should be the repo too — a
  task-named session is unfindable next to its repo-named siblings. **Never
  reuse a handle that already has a registry row, even a stale-looking one**
  (the owner may be alive with a lagging row; a collision makes two sessions
  execute the same briefs in parallel).

  **Invariant: claude NEVER starts in a detached pane.** Its TUI in a pane
  with no attached client **exits cleanly with no error** — a silent failure
  indistinguishable from success until the peer never answers. So no `-d`, no
  detach-then-attach dance: a human creates and attaches in one step (command
  above), and a Claude session / headless context doesn't drive tmux at all —
  it spawns durable peers with `comm-spawn.sh` (workspace mode), where the FE
  autostart provides the attached client and a clean env. (Hand-rolled panes
  also inherit the spawner's `CLAUDECODE` / `CLAUDE_CODE_*` / `AI_AGENT`
  exports, which makes `claude` detect nesting — second reason sessions don't
  hand-roll this.)

- **Ephemeral task agent** — spawn, do one task, report back to the spawner,
  tear down:

  ```bash
  ~/.sot-comm/bin/comm-spawn.sh <name> <repo-path> --expertise "..." --task "..."
  ~/.sot-comm/bin/comm-despawn.sh <name>   # when done
  ```

  Details and the report-back contract are in the **sot-comm** skill.

## Why a skill (not a hardcoded resume prompt)

Keeping the receive-setup here means we iterate on it in one editable place instead
of in each session's launcher or resume config. If a backend tmux session is
launched with a resume command, point it at `/sot-session-start` (or use the `ccb`
launcher) so this runs automatically on every restart.
