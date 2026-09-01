# ADR 0028: Remote comm auto-connect — myhost-anchored reverse SSH tunnels under systemd --user

**Status:** Accepted (implemented + verified end-to-end; codex + win-fe reviewed the plan and converged, 2026-06-27)
**Date:** 2026-06-27

## Context

The sot-comm relay daemon `sotd` runs on **myhost** bound to `127.0.0.1:18743`
(loopback only — see ADR 0027 for the daemon's connection model). The Linux
cohort (myhost + the servers host-b / host-c / host-d) shares one `$HOME`
over NFS, so the *durable* comm layer (the registry, the per-handle inbox
`.jsonl` files, the `comm-*` scripts) is already visible on every box. But the
**live** relay — the instant-wake path that a session's `comm-listen.sh` bridge
and inbox Monitor depend on — needs a TCP connection to `sotd`, and a loopback
bind is unreachable from another machine (verified: `myhost:18743` UNREACHABLE
from host-b).

Result: a `ccb`/`ccbe` session launched on a server could only catch up on its
durable inbox on its next natural turn; it never woke on inbound. In practice the
servers ran **zero** sessions — the registry was 100% myhost — and "the monitors of
host-b are down" was the visible symptom.

The Windows FE already solved the same shape (no local daemon → tunnel `sotd`
from myhost + explicit `SOT_RELAY_ENDPOINT`); the Linux servers had simply never
been wired up.

## Decision

Anchor everything on myhost (the always-on hub where `sotd` lives) and use
**reverse** SSH tunnels, supervised by `systemd --user`.

### Transport — one reverse tunnel per remote
For each remote R, myhost holds:

    ssh -NT -o ExitOnForwardFailure=yes -o ServerAliveInterval=15 \
        -o ServerAliveCountMax=3 -o BatchMode=yes -o StrictHostKeyChecking=accept-new \
        -R 127.0.0.1:18743:127.0.0.1:18743 R

This binds `127.0.0.1:18743` on R and forwards back through the SSH channel to
myhost's loopback `sotd`. **Reverse-from-myhost** (not forward-from-each-remote)
because myhost already holds agent-less outbound SSH keys to every box (verified),
so the credential + lifecycle stay centralized in one template unit and the
servers stay zero-config. The explicit `127.0.0.1` bind is loopback-only on the
remote regardless of its `GatewayPorts` (default `no`).

### Supervision — systemd --user + linger, on myhost
- `~/.config/systemd/user/sot-relay-tunnel@.service` — a **template** unit
  (`%i` = remote host), `Restart=always`, `RestartSec=5`,
  `StartLimitIntervalSec=0` (don't give up after a remote's transient outage),
  enabled for `host-b`, `host-c`, `host-d`.
- `~/.config/systemd/user/sotd.service` — `sotd` itself, `Restart=always`. This
  closes a gap codex + win-fe both flagged: before this, `sotd` was a detached
  `nohup` process (PPID 1) started by `scripts/launch-devenv.sh`, with **no
  supervisor** — a headless myhost reboot would leave the linger-restored tunnels
  forwarding to a *dead* daemon. `scripts/restart-backend.sh` was made
  systemd-aware (diverts to `systemctl --user restart sotd.service` when the unit
  is enabled, else keeps the legacy detached-nohup path) so it no longer races
  `Restart=always`.
- `loginctl enable-linger <user>` — both the daemon and the tunnels are restored
  on boot with no interactive login.

### Endpoint — one shared line
`export SOT_RELAY_ENDPOINT=tcp:127.0.0.1:18743` in `~/.bashrc`, placed **above
the interactive guard** so non-interactive ssh / daemon-spawned bridges see it
too. Because the reverse tunnel normalizes the endpoint to `localhost:18743` on
*every* Linux box (myhost = the real daemon; remotes = the tunnel), one line is
correct everywhere and lets `comm-relay.sh` skip pgrep auto-discovery. This is
the **shared-HOME Linux cluster only**; the Windows FEs have a separate `$HOME`
and keep setting `SOT_RELAY_ENDPOINT` inline in git-bash.

## Consequences

- A `ccb`/`ccbe` session on any Linux server now auto-joins the **live** relay
  with zero per-session config: `/sot-session-start` runs `comm-listen.sh`
  (bridge dials the tunnel) + arms its inbox Monitor, and instant wake works.
- An **idle** remote with no live session holds **zero** `sotd` connections: the
  `ssh -R` tunnel is just an idle SSH process on myhost until a session actually
  dials through it (`sotd` opens a connection on demand). The three always-on
  tunnels cost three myhost-side SSH procs, not three `sotd` conns, and don't
  interact with the ADR 0027 reaper (no idle-disconnect; healthy quiet bridges
  survive).
- Headless-reboot safe: linger restores `sotd.service`, then the three
  `sot-relay-tunnel@*` units, on boot.
- **Verified 2026-06-27:** relay frames sent *from host-b* (both inline-endpoint
  and zero-config-from-profile) reached myhost's `sotd` and woke the myhost session's
  inbox Monitor; the tunnels survived the `sotd` systemd cutover.

### Operational notes
- Add a remote: `systemctl --user enable --now sot-relay-tunnel@<host>` (host must
  be myhost-ssh-reachable with an agent-less key + a `known_hosts` entry, or rely on
  `accept-new` on first contact).
- Check: `systemctl --user status 'sot-relay-tunnel@*' sotd.service`.
- If a remote's `127.0.0.1:18743` is already bound (orphaned tunnel),
  `ExitOnForwardFailure` makes the unit fail+retry until the port frees — visible
  as a restart loop in `systemctl --user status`.
- `scripts/restart-backend.sh` now restarts the systemd `sotd` when the unit is
  enabled; the legacy `--check` staleness report is unchanged.

## Addendum: derived-handle disambiguation ("derived vs explicit")

**Problem.** A session's default sot-comm handle is `<repo-basename>-<host>`
(`comm-join.sh`, `comm-spawn.sh`). Two *different* projects that happen to
share a basename — e.g. `courseA/instructor-materials` and
`courseB/instructor-materials` — derive the identical default handle. The
registry has no collision check, so the second join silently overwrote the
first's row: both sessions ended up sharing one inbox, and routing/status
aliased two unrelated sessions onto one identity.

**The contract: derived names are smart, explicit names are verbatim.**
Every claimed handle — derived or explicit — records its canonical project
root on the registry row (`"root"`) and on the v2 self-file (`root=` line).
This is additive: a row or self-file without `root` predates the feature
and reads back as *unknown root*, which the algorithm below treats as a
collision, not a free pass.

**Transition behavior.** A legacy registry row (from before this feature)
has no `root`, and unknown-root counts as a collision — fail-safe, not
fail-open. So until a legacy row's own session rejoins (which now records
`root`, including via the ordinary `/sot-session-start` join), a *fresh*
session that lands on that same bare handle from a different directory
gets a qualified name instead, with the stderr notice explaining why. This
is deliberate: grandfathering an unknown root in as "assume same project"
would silently re-enable aliasing for exactly the pre-existing collisions
this feature targets.

**Self-file read-side transition — THE complete matrix (dated 2026-09-01,
PR #150; extended round 2 and round 3 — amends the "reads back as unknown
root... a collision, not a free pass" sentence above for the SELF-FILE
half only; the REGISTRY half is unchanged).** Applied to the self-file
literally, "unknown root = collision" turned out to be too strict in
practice: it discarded EVERY self-file written before `root=` shipped —
every long-running session's identity — on its very next comm call ("Not
joined — run comm-join.sh first"), which is strictly worse than the
pane-recycling bug `root=` was added to close. `comm-context.sh`'s read
side implements the matrix below; this paragraph is its rationale — the
script itself carries only short invariant comments pointing here.

*Third self-file line (`root=...`):*
- present, matches this project's canonical root → accept.
- present but empty, OR present and not a `root=...` line at all (e.g. a
  corrupted `rootBROKEN` — distinguished from "no third line" by array
  length, not just pattern match) → discard unconditionally. Both are
  "evidence present but bad", never routed through the more permissive
  legacy path below (which is only for files that predate `root=`
  entirely).
- absent (no third line at all) → legacy file, evaluated below.

*Legacy self-file (no third line), `repo=` line vs. this project's
basename:*
- mismatched → discard (the original pane-recycling protection: a
  recycled tmux pane id, a genuine `cd` elsewhere, or a shared no-pane
  self-file read from a different repo/cwd). This check runs BEFORE any
  registry consultation, by design — cheap, purely local, and a
  DIFFERENT repo's identity is never rightfully ours no matter what the
  registry says about the name on line 1 (registry precedence here was
  proposed and refused in round 3: no field case motivates a new branch
  for it).
- matching, or absent entirely (the ancient one-line format, pre-#68) →
  consult the registry for the handle on line 1:
  - the registry could not be read/parsed at all (malformed JSON,
    unreadable file, an NFS hiccup) → discard, NO HEAL, NO WRITE.
    Treated as NO EVIDENCE, never as "no row" — the failure is transient
    by nature, and the next call re-reads.
  - registry row has a nonempty root, MATCHES this project → heal.
  - registry row has a nonempty root, DISAGREES → discard (a basename
    match can never outrank contrary registry evidence — this is the
    wrong-checkout certification the registry consultation exists to
    prevent).
  - registry row has no root (legacy row) or no row at all → no contrary
    evidence, so:
    - `repo=` present and matching, self-file is PANE-KEYED (each tmux
      pane gets its own path, not the shared nopane slot) → heal on the
      basename match alone. Residual, deliberately-accepted,
      time-bounded ambiguity: a same-basename different-directory repo
      assigned this exact recycled pane id, with no registry row to
      catch it either, would also heal here — rare, and stops mattering
      once the fleet has cycled once post-upgrade.
    - `repo=` present and matching, but this IS the shared nopane slot
      (`"$HOST__nopane.txt"`, read/written by every no-tmux-context
      shell on the host, in every repo, forever) → discard. A basename
      match alone is not enough evidence for a slot this widely shared;
      it needs the same registry corroboration the no-evidence branches
      above already require.
    - `repo=` absent (ancient one-line) → discard; it carries no
      evidence of its own to check against.

Only a name that comes from **derivation** (nothing else was supplied) runs
the disambiguation algorithm. A name from `--name`, `$SOT_COMM_NAME`, or an
already-joined self-file identity is always used **verbatim** — no
escalation, no rewriting, same overwrite-on-collision behavior as before.

The derived-name algorithm takes an explicit **mode**, checked against the
registry's current state at each tier:

- **reclaim** (`comm-join.sh` — an ordinary join) — a tier is claimable if
  it's unclaimed, OR if the existing row's root already equals mine
  (today's same-root reclaim/rejoin).
- **fresh** (`comm-spawn.sh` — creating a NEW agent) — a tier is claimable
  ONLY if it's genuinely unclaimed. An existing row — even one sharing the
  spawning session's own project root — is someone/something else's from
  spawn's point of view and is never reclaimed: spawning a second agent
  against a project that already has a live one must not silently
  overwrite that live agent's tmux/pane/status fields.

The three tiers themselves are the same shape under either mode:

1. `<basename>-<host>` — claimable per the mode's rule above.
2. Else `<basename>-<parentdir>-<host>` — `parentdir` is the basename of
   the project root's *parent* directory (human-meaningful: the
   course/collection/workspace name), same claimability rule. Example:
   `courseB/instructor-materials` becomes `instructor-materials-courseB-<host>`.
3. Else `<basename>-<hash6>-<host>` — `hash6` is the first 6 hex characters
   of `sha256(canonical root path)` — checked with the SAME claimability
   rule as tiers 1-2, not claimed unconditionally. If tier 3 isn't
   claimable either (an explicit owner already sits on that exact
   hash-qualified name, or — vanishingly unlikely — two different roots
   share a six-hex prefix), derivation FAILS LOUDLY asking for an explicit
   `--name`. There is no tier 4 and no overwrite of anything, ever.

Each escalation prints one line to stderr naming the handle that was
already held, by which root, and what was joined instead; the final
`Joined sot-comm as @<handle>` line stays the authoritative confirmation.
A repo/parentdir/host component that isn't already in workspace.create's
charset (`[A-Za-z0-9._-]`, max 64 chars) is sanitized and length-clamped
*before* any candidate is composed, so a derived handle can never diverge
from what workspace.create will accept and no raw path text reaches a
shell command unsanitized; a host whose sanitized form had to change at
all (truncated, or characters rewritten) gets a short digest of the raw
host appended, so two different real hosts that happen to sanitize to the
same string can't alias onto one tier-1 identity when they share a root.

**Liveness is deliberately not consulted.** The comparison is root-only —
a stale registry row (its session long gone) still holds its claim until
an existing cleanup path removes it. This keeps the rule one-dimensional
and prevents a handle from flip-flopping between two projects depending on
which one happens to have a live session at any given moment.

**`comm-spawn.sh`'s `<name>` is optional.** Omitted, it derives and
disambiguates in **fresh** mode (one positional argument = `<repo-path>`;
the legacy two-positional `<name> <repo-path>` form, and `--name`, remain
verbatim). When derivation had to qualify past the bare tier, the spawn
also passes a qualified `--display-label` (`<basename>-<qualifier>`) so
the two session-strip rows in the frontend stay visually distinguishable —
and that auto-composed label is itself checked (by normalized slug,
mirroring `rust/backend/src/paths.rs::slug`) against the daemon's existing
workspace list before the workspace is created, refusing rather than
risking a same-slug rebind of someone else's workspace (e.g. a worktree's
`<repo>-wt-<short>` grouping label). That check is a best-effort,
list-then-create human-UX guard, not an atomic guarantee.

A provisional registry row makes a spawned agent addressable immediately,
before it has actually joined; if anything synchronous afterward fails
(the daemon unreachable, workspace creation rejected, the launch itself
failing), that row is rolled back — but only if it's *still provably the
exact row this spawn wrote* (matched by root + a random nonce +
`status:"spawning"`), never a row that a real join or another claimant has
since replaced.

**Implementation.** One shared helper, `sot_derive_handle` (`comm-lib.sh`),
is the single home for the algorithm; `comm-join.sh` and `comm-spawn.sh`
both call it rather than deriving `<repo>-<host>` themselves. Deriving a
name and registering it are not two separate steps: `claim_derived_handle`
runs both under the registry's single lock, so a concurrent derived claim
for a different root can never land in the gap between "decide" and
"write" — the exact aliasing bug this feature closes, which would
otherwise survive as a race window (comm-spawn.sh in particular drives
many joins back-to-back for bulk workspace bring-up). That lock itself
fails CLOSED: if it's still held after a bounded wait (~10s), the caller
gets a clear error naming the lock path and the holder's age rather than
forcing a takeover, which could let two writers corrupt the registry file
or reopen the very race the lock exists to close. Tests:
`comm/core/tests/test-join-disambiguation.sh` (hermetic — a temp
`$SOT_COMM_HOME`, a temp self-file per simulated session, and an isolated
tmux server for the spawn case; never touches a real install).
