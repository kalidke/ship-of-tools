# ADR 0036: Workspace lifecycle hygiene — refuse duplicate roots, reap orphans

**Status:** Accepted (2026-07-30). Phase 1 (duplicate-root gate) built with this
ADR; Phase 2 (orphan reap) designed here, implementation deferred.
**Date:** 2026-07-30

## Context

Two long-standing gaps in what workspace registrations the daemon accepts and
how long it keeps them:

1. **Duplicate roots.** `workspace.create` validated that `project_root`
   exists and is a directory — nothing else about it. A second workspace
   pointing at an **already-registered project root** registered fine, wrote
   its own TOML, and from then on was faithfully respawned (tmux session and
   all) on every daemon boot. Seen live 2026-07-30: an agent session hunting
   for a peer created a duplicate workspace for a root that was already
   registered under a different label. The duplicate outlived the agent that
   made it and came back after a daemon restart, because the on-disk
   registration — not the agent — was now its source of truth.

   Duplicate roots are not a cosmetic problem. Two workspaces on one checkout
   means two agent sessions sharing one working tree — independent `git
   checkout`s under each other, racing index/lock files, and two "drivers"
   for one project (the collision class already documented for worktrees,
   which exist precisely to give parallel sessions *separate* roots).

2. **Orphaned registrations.** Workspaces torn down outside the blessed path
   (`workspace.destroy` via the despawn/worktree-clean scripts) leak: a hard
   kill, a deleted worktree root, or an ad-hoc test daemon leaves a TOML +
   dead tmux target that the daemon re-adopts forever. A manual sweep
   (2026-07-12) collected several of these; nothing prevents re-accumulation.

Both are the same policy question — *what registrations does the daemon
accept, and for how long does it honor them* — so they share this ADR. They
land separately (see Phasing) because one is a cheap acceptance check and the
other deletes state on a timer.

## Decision

### Phase 1 (this change): `workspace.create` refuses duplicate roots

On create, canonicalize the requested `project_root` and compare it against
the canonical root of every registered workspace — except the daemon's inert
default anchor (ADR 0042 amendment; it is not a session and never runs an
agent, so a real session at its root is not this collision):

- **Match with a different slug → refuse** with a structured error:

  ```json
  { "error": "project_root is already registered as workspace '<label>' (slug '<slug>')",
    "code": "duplicate_root",
    "existing": { "workspace_id": "…", "slug": "…", "label": "…" } }
  ```

  The `existing` block is the point: the caller (FE session-create,
  `comm-spawn`) can offer "switch to the existing workspace" instead of
  dead-ending on an error string.

- **Match with the same slug → allow.** A same-slug create has always been an
  id-preserving metadata refresh (`Workspaces::insert` keeps the workspace_id
  and lets new metadata win); boot and spawn flows rely on that idempotence.
  The gate only rejects a *second identity* for one root, never a re-create
  of the same identity.

- **Canonicalization failure → skip the gate** (log a warning, proceed).
  The candidate was already `exists()`+`is_dir()`-checked, so a failure here
  is exotic (permissions, racing unlink). Prevention must not make creation
  less reliable than it is today. A registered workspace whose root no longer
  canonicalizes (deleted dir) is skipped for the same reason — judging *that*
  workspace is Phase 2's job, not the create path's.

Comparison is by canonical path, both sides resolved at check time: symlinked
spellings of one directory must collide, and nothing about stored state
changes shape (roots stay as the user supplied them; only the comparison
canonicalizes).

**No `allow_duplicate` escape hatch.** The one legitimate "second session on
the same code" pattern is a git worktree, which has a distinct root by
construction and passes the gate untouched. Deliberately parallel sessions on
one tree is the failure mode, not a use case ("defer until forced").

**Not changed:** the daemon recreating tmux sessions for registered
workspaces at boot. That behavior is what makes registrations durable and is
correct; the fault was accepting the bogus registration, not honoring it.

### Phase 2 (deferred): daemon-side orphan reap

The daemon's existing per-tick pane-state task already visits every
workspace's tmux session. Extend it: a workspace whose tmux session no longer
exists AND whose kernel is not running AND with no client attached AND older
than a boot-grace window (so a still-starting autostart isn't reaped) is
auto-`workspace.destroy`ed. Companions: unlink a stale session socket on
daemon bind (also fixes the rebind block), and guard legacy state-dir
migration from re-adopting a TOML whose tmux/socket are already dead.

Deferred because it destroys state on a timer: the grace-window semantics
deserve their own review and tests, and a bug here deletes real workspaces.
Phase 1 is pure prevention and ships first.

## Consequences

- The incident class is closed at the door: a duplicate registration can no
  longer be created, so it can no longer persist or respawn.
- Callers get a machine-readable `duplicate_root` + the existing workspace's
  identity — better UX than success-then-mystery-row.
- A same-root create now costs one `canonicalize` per registered workspace
  (a handful of syscalls; creates are rare and interactive).
- Scripts that blindly `workspace.create` an already-registered root now see
  an error where they previously saw a duplicate row appear. That error is
  the fix working; the `existing` payload tells them where to go.
- Until Phase 2 lands, orphans still require the manual sweep.

## Verification

- Unit tests on the gate helper: same root under a different slug is found
  (including via a symlinked spelling, unix-only test); same slug is not
  (refresh stays allowed); different roots pass; a registered root that no
  longer resolves is skipped rather than fatal.
- `cargo test -p sot-backend` green.
