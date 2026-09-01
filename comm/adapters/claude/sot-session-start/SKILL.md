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

**Step 0 decides whether the rest runs at all.** If you SURVIVED a **context
wipe** (a compaction *or* a `/clear`) you are still fully connected — skip
everything below. Otherwise (a cold start or a `claude --continue` restart) you
are deaf: do the three bootstrap steps — **(a)** join (also your identity);
**(b)** in parallel, start the listener + arm the inbox Monitor + catch up on the
down-window; **(c)** one post-arm selftest proves the chain wakes you.

### (0) First: did you SURVIVE a context wipe (compaction / `/clear`), or genuinely (re)start?

This skill is the **deaf-restart** bootstrap. A cold start or a `--continue`
restart genuinely kills your harness Monitor, so the full (re)join / listen /
arm / catch-up below is exactly right. But **losing context does not make you
deaf** — your Monitor and listener are background tasks that survive both a
compaction *and* a `/clear`. Neither kills the session process, so the watcher
child keeps running and keeps delivering (measured 2026-07-25 on a `/clear`ed
backend session: the watcher was still parented to the live claude pid, and a
post-clear `comm-listen.sh --selftest` woke the cleared context). Running the
full bootstrap on a merely context-wiped session is actively harmful: it
**double-arms** the Monitor (duplicate wakes, compounding per wipe), **replays**
already-handled messages via `comm-poll`, and **wipes your live work-state** via
`comm-join`'s row-replace. So branch first.

You normally arrive here from a `SessionStart` hook directive that names the
wipe (`comm-postcompact-reminder.sh` for a summary, `comm-postclear-reminder.sh`
for a `/clear`), but the pgrep below is the arbiter either way — trust it over
your own guess about why you're here.

Get your handle and check for a **live watcher**:

```bash
eval "$(~/.sot-comm/bin/comm-context.sh 2>/dev/null)" 2>/dev/null || true   # sets NAME (empty when not joined) — eval, do NOT sed-scrape: values are %q-quoted, so a scrape can capture literal quotes as a bogus non-empty handle
if [ -n "$NAME" ]; then
    h="$NAME"
else
    # Never hand-construct <repo>-<host> yourself (Codex review round-3
    # finding 6) — sanitization, truncation, and the long-host digest
    # suffix (ADR 0028's host-alias guard) can all change it. Compute it
    # the SAME way comm-join.sh would, via the existing tier-1 derivation
    # (comm-lib.sh's sot_derive_handle, third output line):
    source ~/.sot-comm/bin/comm-lib.sh
    h="$(sot_derive_handle reclaim "$PROJECT_ROOT" "$HOST" 2>/dev/null | sed -n 3p)"
    h="${h:-$(basename "$PWD")-$(hostname -s)}"   # last-resort only if derivation itself fails
fi
h_re="$(printf '%s' "$h" | sed 's/\./\\./g')"   # escape dots — repo names contain them (e.g. MyOrg.github.io-myhost); an unescaped '.' matches ANY char and could false-match a sibling
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

- **Prints a PID → you SURVIVED the context wipe** (compaction or `/clear`).
  You are still connected —
  **STOP: do NOT run steps (a)–(c).** Re-reading this doc (and the `/sot-comm`
  skill) has already restored your operating context — handle, the send/poll/
  status verbs, the work-state rules — which is the whole point of re-running on
  a wipe. You keep receiving on the watcher that never died; re-arming,
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

**Branch BEFORE joining** (Codex review round-2 finding 6/E) — the bare
no-args form below is only for a **genuinely new** session. Determine your
canonical handle first — never hand-construct `<repo>-<host>` yourself
(Codex review round-3 finding 6: sanitization, truncation, and the
long-host digest suffix can all change it). Reuse `$h` from Step 0 above
if you just computed it there; otherwise:
```bash
eval "$(~/.sot-comm/bin/comm-context.sh 2>/dev/null)" 2>/dev/null || true
source ~/.sot-comm/bin/comm-lib.sh   # for sot_derive_handle / sot_bridge_running_for
CANONICAL="${NAME:-$(sot_derive_handle reclaim "$PROJECT_ROOT" "$HOST" 2>/dev/null | sed -n 3p)}"
```
Then check either signal:
- Have you (this session, this repo) held `$CANONICAL` before — from a
  prior turn's "Joined sot-comm as @..." line, this repo's own notes, or
  general knowledge that this repo already runs a durable session? **or**
- Is a listener bridge for it already running under your uid — the SAME
  shared detector comm-join.sh's own stranding guard uses (it also catches
  a directly-started bridge with no tmux marker, which a raw
  `tmux has-session` probe would miss):
  ```bash
  sot_bridge_running_for "$CANONICAL" && echo "bridge already running for $CANONICAL"
  ```

**If EITHER is true, join explicitly and skip the bare form entirely:**
```bash
~/.sot-comm/bin/comm-join.sh --name "$CANONICAL"
```
(See "Identity evicted / wrong handle after a rejoin" below for exactly why
the bare form is unsafe here — it derives a handle from scratch, sees your
own now-stale row as "held by an unknown project", and escalates AWAY from
it, which is how sessions get stranded in the first place.)

**Only when NEITHER is true** (no prior handle for this repo, no bridge
running — a genuinely fresh session) use the bare form:

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
this on a genuine cold start / restart — a session that survived a wipe stopped at
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

## Identity evicted / wrong handle after a rejoin

Symptom: a comm call that used to work suddenly says **"Not joined — run
comm-join.sh first"** for a session that has been running (and joined) the
whole time — nothing about the session changed, only its on-disk self-file's
validation did (a stale/pre-upgrade self-file, or a genuinely recycled tmux
pane). Or worse: you already reacted to that by running a **bare**
`comm-join.sh`, and it printed a **different** handle than the one this
session, its peers, and any dashboard have always known it by (e.g.
`<repo>-<host>` becoming `<repo>-<parentdir>-<host>`).

**The no-arg `comm-join.sh` is the WRONG move for a session that previously
held a handle.** No-args derives a handle from scratch; derivation sees your
own canonical handle's row as "held by an unknown project" (your own
now-discarded row looks exactly like a collision from the outside) and
escalates AWAY from it — which is how you got stranded in the first place.
Never rejoin bare to "fix" an identity problem if you used to have a name;
reclaim it explicitly instead. (`comm-join.sh` itself now warns loudly, at
the moment of escalation, when a listener bridge for the bare handle it's
escalating away from is still running under your uid — treat that warning as
this exact situation and follow its printed recipe.)

Recipe (validated live against a real 28h-stale-row incident):

1. **Prove sole ownership of the canonical handle before reclaiming it** — a
   real collision (someone else's live session) looks identical to your own
   stranded identity from the outside. Query the **canonical** handle
   specifically (Codex review round-2 finding 6/E) — not the accidental one
   you're currently joined as; `--status` with no `--name` reports on
   whatever you're joined as RIGHT NOW, which at this point in the recipe
   is still the wrong one:
   ```bash
   ~/.sot-comm/bin/comm-listen.sh --status --name <canonical-handle>
   # or, for the raw tmux marker directly (source comm-lib.sh first — sot_tmux_socket is defined there, not a standalone binary):
   source ~/.sot-comm/bin/comm-lib.sh
   tmux -S "$(sot_tmux_socket)" has-session -t "=commbridge-<canonical-handle>"
   ```
   Confirm: exactly one live session has this repo as its cwd, and that
   bridge's creation time matches when *this* session actually started. If
   you can't confirm sole ownership, stop and ask a human — reclaiming
   someone else's live handle strands *them* instead of fixing you.
2. Drop the accidental/escalated handle:
   ```bash
   ~/.sot-comm/bin/comm-leave.sh --name <accidental-handle>
   ```
3. Reclaim the canonical handle **explicitly** (never bare — see above):
   ```bash
   ~/.sot-comm/bin/comm-join.sh --name <canonical-handle>
   ```
   `--name` is always used verbatim; this is the one case a plain rejoin
   cannot do, since bare derivation is exactly what stranded you.
4. Your listener bridge almost certainly never needed to move — it was
   bridging the *correct* (canonical) handle's inbox the entire time, just
   unaddressed while your registered identity pointed elsewhere. Confirm
   it's still up rather than starting a redundant one:
   ```bash
   ~/.sot-comm/bin/comm-listen.sh --status
   ```
5. **Selftest is required, not optional** — prove the wake path actually
   reaches you under the reclaimed name:
   ```bash
   ~/.sot-comm/bin/comm-listen.sh --selftest
   ```
   Require the **Monitor notification** (`[relay] from __selftest__: …`),
   not just the inline `receive path OK` — the notification is what proves a
   peer's *next* message actually reaches this session, not just that a file
   got written.

This is a rare recovery path, not a routine step — most sessions never hit
it, because `comm-context.sh` now self-heals a legacy (pre-root=) self-file
on read instead of discarding it. You land here only if you already rejoined
bare *before* noticing, or a case root= validation still (correctly) rejects
— e.g. a genuinely different project sharing this repo's basename+host, or a
legacy self-file whose handle the sot-comm *registry* already corroborates
against a **different** root (comm-context.sh consults the registry before
ever healing a basename match, and refuses outright on a disagreement — a
basename can never outrank contrary registry evidence).

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
  # -S: sot sessions live on the private per-user tmux server (the ADR 0038
  # keeper socket) — a bare `tmux` would create the session on the DEFAULT
  # server, invisible to the daemon and the FE Sessions list.
  # Resolve the path with `sot_tmux_socket` — never hand-roll it. It asks
  # `sotd tmux-socket-path` (the single source of truth) and only mirrors its
  # tiers when sotd isn't on PATH, which is the common case in a human shell.
  source ~/.sot-comm/bin/comm-lib.sh
  SOCK="$(sot_tmux_socket)" || { echo "cannot resolve the sot tmux socket" >&2; exit 1; }
  tmux -S "$SOCK" new-session -s <tmux-name> -c <repo-path> ~/.local/bin/ccb   # no -d: create AND attach
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
