# ADR 0048: the filer's receipt is the cross-box delivery verdict

**Status:** accepted; implemented in the comm scripts, the daemon and the
frontend (the scripts deploy via `update_comm`; the daemon and frontend halves
ride a release).

## Context

`comm/PROTOCOL.md`'s own rule is that the append IS the delivery: a frame in a
handle's inbox is read at that session's next turn boundary, awake or not. For a
target this box's registry names, the sender does the append itself, so `filed
-> @h` is a fact.

For a target it cannot name, the frame goes over the wire and somebody else does
the append — a peer's relay bridge, or the frontend on a box that hosts sessions
with no shared `$HOME`. The sender never learned what happened there. Every
verdict it printed was derived from the daemon's `receivers` roster instead:

- a receiver whose declared name equalled the target was reported as a delivery,
  though a named connection is not proof of an append;
- a frontend receiver whose declared host the target's name ENDED IN was
  reported as a probable filer — a name-suffix guess that matched a misspelled
  handle identically, and once reported `filed via the frontend (unconfirmed)`
  with exit 0 for a send nobody had filed.

A prior attempt had the frontend DECLARE the handle set it files for
(`fe.files_for`). Its set builder, gate and tests shipped; its caller was ruled
out before release, because the declaration only ever reached a daemon on the
frontend's own box — which already knows those handles from their `agent.join` —
while the remote hub that answers a directed send never saw it. It bought
nothing. This ADR is that ruling's discharge, not its reversal.

## Decision

**Whoever appends the frame says so, and that statement is the verdict.**

1. **The frame carries a sender-minted opaque `id`** (`agent.send`, optional on
   the wire). A receipt is attributable to exactly the frame it acknowledges;
   without it, two concurrent sends to one handle can swap verdicts and a single
   success can vouch for a failure.

2. **The filer answers `agent.filed {id, handle, filed, reason?}`** on its own
   connection. The daemon stamps `filer` **from that connection's declared hello
   `name`** and fans the result out as an `agent.receipt` evt. The request has no
   `filer` field at all, so one filer cannot vouch under another's name; a
   connection that declared no name is refused `bad_filer`, because an anonymous
   vouch is indistinguishable from a forged one.

3. **The sender reads its own receipt** on the connection it is already holding,
   bounded by the existing 5 s transport bound — no new timeout knob. The
   `receivers` roster is demoted to a diagnostic in the one failure line that
   names who was attached and did not answer.

4. **The daemon keeps no delivery state.** It relays a receipt exactly as it
   relays a message: no pending table to leak, expire or lie from.

5. **Who claims what.** A relay bridge IS its handle, so it claims `filed: true`
   after an append that returned 0, and claims nothing when the append failed. A
   frontend appends every inbound frame into one inbox of its own regardless of
   `to`, so "I appended" is not "I filed for @h": it claims `filed: true` only
   for a handle one of its OWN-host rows declares, `filed: false` with a reason
   for a handle absent from that set (the fleet's only answer to "is that remote
   handle real"), and **nothing at all** when it has no own-host list — "the set
   is known and lacks @h" and "there is no set" are different facts, and only the
   first may be reported as an absence.

6. **The append is never gated on the claim.** The frontend keeps appending
   unconditionally. Gating the append on the declared set would make a box deaf
   the moment a row's `agent.join` is late: the claim may be conservative, the
   filing may not.

7. **`fe.files_for` is deleted from the wire** — op const, both payload structs,
   the frontend's sender and its outgoing variant. The claim now travels back
   over the link the frame arrived on, so it cannot miss the hub by construction.
   The set builder (`files_for_from_rows`) and its own-host gate
   (`declares_files_for`) survive with their tests, read per inbound frame as
   this frontend's local decision about what it may claim.

8. **Both comm verbs route.** `comm-relay.sh send` already execs `comm-send.sh`
   on a registry hit; `comm-send.sh` now execs `comm-relay.sh send` on a
   directed-send registry MISS instead of refusing `no such handle`. The two
   triggers are mutually exclusive (hit → file, miss → wire), so no recursion
   guard is needed, and a session on a machine that shares no `$HOME` can never
   again be told "no such handle" about a peer it is structurally incapable of
   naming. A `--broadcast` fans out over registry keys and never takes this path
   — an `exec` mid fan-out would abandon the remaining targets.

Nothing about the transport changes.

## Consequences

Verdicts, in the order the first that applies wins:

| what came back | verdict | exit |
|---|---|---|
| no ack, `ok != true`, or no `receivers` array | `ERROR: unreachable, nothing filed` | 1 |
| `--all` | `relayed -> <all> (N receiver(s))` | 0 |
| `receivers` empty | `no such handle: h` | 1 |
| ack without an `id` | `NOT CONFIRMED: this daemon predates filer receipts` | 1 |
| receipt, `filed: true` | `filed -> @h (by <filer>, relay)` | 0 |
| receipt, `filed: false` | `no such handle: h — <filer> reports: <reason>` | 1 |
| no receipt before EOF or the 5 s bound | `NOT CONFIRMED: sent for @h; no filer answered in 5s. Attached: …` | 1 |

Everything a receipt cannot exist for is decided on the ack — a broadcast, a hub
that dropped the id, an empty roster — so no path waits out five seconds for an
answer nobody can give.

**Mixed fleet.** No struct here uses `deny_unknown_fields`, and both new fields
are omitted when absent, so: a new sender against an old hub gets `predates
filer receipts` on the ack (no false success, no stall); an old sender against a
new hub publishes no id, nothing claims, and it behaves exactly as its own
version always did; a new hub with an older frontend as the filer appends the
frame as today and the sender reports NOT CONFIRMED naming it; a new frontend
against an old daemon has its `agent.filed` answered as an unknown op, which the
unmatched-id fallthrough drops silently (no per-frame warn). Cross-Linux sends on
a shared `$HOME` are registry hits and never touch the wire at all.

**What this deletes.** The name-suffix guess and its "probably its filer"
reasoning; the roster-name success branch; `fe.files_for` on the wire; the "no
such handle" dead end for a target this box cannot name, and with it the
requirement that a caller know which verb routes; and the instruction that only
a reply proves the path.

**What it deliberately does not do.** A `(handle, host)` address on every frame
would answer a question the receipt already answers better, and would let a
sender silently pick one of two boxes claiming a handle instead of reporting who
actually filed; if duplicate handles become real, the fix is the naming rule plus
two visible receipts. A frontend that answers `filed: false` for a handle whose
session has simply not started yet is harsher than the truth — the frame IS
still appended and a later session reads it — and that is accepted for now
because "check the spelling" is right far more often; it is revisited when the
frontend files per handle rather than into one shared inbox, where "not started
yet" becomes provable.
