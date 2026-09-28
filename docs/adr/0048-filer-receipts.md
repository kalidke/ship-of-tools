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

2. **The filer answers `agent.filed {id}`** on its own connection — "I appended
   the frame carrying this id", which is the whole claim. The daemon stamps
   `filer` **from that connection's declared hello `name`** and fans the result
   out as an `agent.receipt {id, filer}` evt. The request has no `filer` field,
   so a receipt always names something checkable against the roster; a
   connection that declared no name is refused `bad_filer`, since a receipt
   naming nobody puts the verdict back where the guess was.

   **`filer` is attribution, not authentication.** Nothing validates a hello
   `name`, and the frame id reaches every client in the `agent.message` fan-out,
   so any authenticated client could vouch under any name. That is accepted:
   every client in this fleet is ours, a hostile client is not the threat model,
   and validation machinery for one would be complexity bought for nothing. What
   the receipt buys is a claim from something that says it did the work, in
   place of a name-suffix guess made by the sender.

2a. **There is no negative claim, and this is load-bearing.** `agent.message` is
   fanned out to EVERY connection with no `to` filter, so a "did not file"
   answer would be a global assertion made from local knowledge: every attached
   frontend would deny a handle it does not host, the sender's read loop breaks
   on the first receipt carrying its id, and a frame that WAS filed would report
   `no such handle` — the defect this ADR exists to remove, with the sign
   flipped, on the common path. Absence of a receipt is the only negative, and
   it is the sender's own conclusion. It also makes break-on-first-receipt
   sound: the only multi-receipt case left is two positives, and either is true.

3. **The sender reads its own receipt** on the connection it is already holding,
   bounded by the existing 5 s transport bound — no new timeout knob. The
   `receivers` roster is demoted to a diagnostic in the one failure line that
   names who was attached and did not answer.

4. **The daemon keeps no delivery state.** It relays a receipt exactly as it
   relays a message: no pending table to leak, expire or lie from.

5. **Who claims what.** A relay bridge IS its handle, so it claims after an
   append that returned 0, and claims nothing when the append failed. A frontend
   appends every inbound frame into one inbox of its own regardless of `to`, so
   "I appended" is not "I filed for @h": it claims only for a handle one of its
   OWN-host rows declares, and stays silent otherwise. Those rows are resolved
   through the connection's DECLARED host, never by looking up the frontend's
   hostname in the workspace-list map — that map is keyed by the dial key, and a
   box whose own daemon is dialed under any other label would claim nothing,
   ever, in a way no pure unit test over injected rows can see.

   A nav-envelope message (`sot_ui`) is acted on and never appended, so it is
   never claimed either: the claim sits in the same branch as the append, not
   beside it.

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

8. **Both comm verbs route, and both doors ask the same question.**
   `comm-relay.sh send` already execs `comm-send.sh` on a registry hit;
   `comm-send.sh` now execs `comm-relay.sh send` on a directed-send registry
   MISS instead of refusing `no such handle`. A session on a machine that shares
   no `$HOME` can never again be told "no such handle" about a peer it is
   structurally incapable of naming.

   The exclusivity that makes this safe is not free: the relay used to ask "does
   a row exist" while send asked "does the row name a host", so a row that
   existed with an empty or absent `host` was a hit for one and a miss for the
   other, and the pair span forever, leaking a temp file per lap (the `exec`
   discards the `EXIT` trap, so that file is also dropped by hand before it).
   Both now ask the one question that decides whether a local append and poke
   are possible: `.host` non-empty. A recursion guard would have survived the
   loop; agreeing predicates delete it. A `--broadcast` fans out over registry
   keys and never takes this path — an `exec` mid fan-out would abandon the
   remaining targets.

### Amendment: one box, two frontends, one append

The daemon fans every `agent.message` out to every open connection, and each
attached frontend appends unconditionally into one `fe-inbox.jsonl` whose path
has no per-frontend component. Two frontends on one box therefore file the same
frame twice, and the reader — a line-offset cursor with no dedupe — shows it
twice and wakes the session twice. Items 9–14 close that.

9. **First-writer-wins is decided at the inbox file, before the write.** The
   frontend takes an exclusive lock on the inbox file, reads a bounded window
   back from its end, and appends only when no line in that window already
   carries the same `(id, to)` pair. The lock is released immediately after.
   This is arbitration placed where it can actually gate a write: locally,
   synchronously, with no peer involved.

   The key is `(id, to)` — decision 1's sender-minted opaque id, plus the
   frame's addressee, which is the target row's handle. Both are already in the
   fanned-out payload; nothing is added to the wire.

   **A frame carrying no `id` is appended unconditionally, as before.** This is
   deliberate. The id is the only field that distinguishes two sends; the rest
   of the payload is byte-identical across frontends *and* would be
   byte-identical for a genuine resend of the same text to the same handle in
   the same second, because the daemon stamps `ts` once and the frontends
   re-serialise what they were given. Deduping such a frame by content would
   collapse a retry — a lost message, which is worse than the duplicate it
   would prevent. A sender too old to mint an id keeps its current behaviour on
   a path that already reports `NOT CONFIRMED`, and decision 2a's `receipt_for`
   already declines to claim for it.

   **Every failure fails open: append anyway.** Lock not granted within the
   deadline, lock error, window read error, unparsable window — all append. A
   duplicate is a nuisance the reader survives; a drop is a message nobody ever
   sees. The failure direction is named here so no later refactor can quietly
   reverse it.

   The window is a heuristic and only a heuristic. Under the lock protocol the
   bytes separating two copies of one frame are only the bytes some lock holder
   appended during the contention window — a handful of lines in every
   realistic case, since two frontends handed the same broadcast frame enter
   the lock within milliseconds of each other. The broadcast's own capacity
   bounds something different and still useful: a frontend whose subscription
   falls far enough behind is reported lagged and those frames are skipped,
   never written, so one frontend can never be arbitrarily far behind its
   sibling. A separation larger than the window falls through to the append,
   which is the pre-amendment behaviour.

10. **The claim follows the FILE, not the write.** The append reports "the frame
    carrying this id is in this box's inbox" — true when this call wrote the
    line, and equally true when this call read the line there under the lock and
    therefore skipped its own write. A frontend claims when that is true and
    `receipt_for`'s own-host gate passes; it stays silent otherwise.

    This widens what decision 2's claim asserts, and the widening is deliberate.
    It read "I appended the frame carrying this id". It now reads: **"the frame
    carrying this id is in the inbox I file into, and a row of my own host
    declares its addressee."** The value decision 2 was protecting is unchanged
    — the receipt is a statement about something the claimant observed, not a
    name-suffix guess made by the sender — and "I read this frame in the file I
    file into, holding the lock on it" is exactly as observed as "I wrote it."

    The narrower "I wrote it" cannot be used, because it can produce **zero
    receipts for a frame that was filed.** `receipt_for` depends on the
    frontend's own-host row list, which is filled per frontend process on its
    own `workspace.list` reply; two frontends on one box have independent dial
    sets and reply timing, so a just-attached frontend can win the lock, write
    the line, and have no row list to claim from, while its sibling finds the
    duplicate and stays silent. The matrix reports that outcome as a false
    failure. A duplicate receipt is not: decision 2a's break-on-first already
    makes the second one free.

    `filer` attribution is unaffected. Two frontends on one box declare the same
    hello `name`, so a receipt names the box's frontend address either way —
    which decision 2 already accepts as attribution, not authentication.

11. **Decision 6 stands, restated.** "The append is never gated on the claim" is
    unchanged: no append here waits on a verdict from any other process. What
    gates the append is the *file's own content* — a local read of the record
    itself. The distinction is the whole amendment. A gate that can only
    suppress a write whose effect is already on disk cannot lose a message; a
    gate that waits on a peer can.

12. **The daemon keeps no delivery state.** Decision 4 is untouched.
    `handle_agent_filed` stays stateless, `agent.filed` stays one field, and
    nothing is added to any wire struct. A daemon restart cannot lose
    arbitration state, because no daemon holds any.

13. **Anything typed on delivery is emitted only by the process that put the
    frame in the file.** The frontend types nothing on delivery today; the gated
    poke is the sender's, on a route that never involves a frontend. When filing
    and poking do meet — ADR 0047's closing direction, the daemon filing for its
    own rows — the poke sits in the same branch as the file outcome, never
    beside it, exactly as decision 5 already requires of the claim. A frontend
    that skipped its write because the frame was already there must not type:
    the frame's own filer already did, or will.

14. **The append never runs on the frontend's UI thread.** The lock wait and the
    window read are filesystem operations with a bounded but real wait, and the
    delivery branch sits inside an unbounded drain on the winit main thread,
    where a wait stalls the event loop and a burst multiplies the stall. They
    run on one dedicated filer thread per frontend process, fed by an unbounded
    FIFO channel, which performs the append and — once it knows the file outcome
    — sends the claim itself. The UI thread's whole cost per frame is the pure
    claim computation and a non-blocking push.

    One thread and a FIFO channel means a frontend appends frames in the order
    the daemon sent them, which the line-offset reader needs: a pool or a thread
    per frame would reorder a conversation and multiply contention on a lock
    whose entire purpose is one writer at a time. The channel is unbounded
    because a bounded one either blocks the send — putting the wait back on the
    UI thread — or drops, which loses a message. Every exit path drains it with
    a bounded join; a hard kill loses whatever is still queued, and that is
    honest, because no claim was sent for those frames.

    The lock is exact where the file is read: that platform's state dir is
    machine-local storage. On a box with a network home the lock may be weaker,
    and there the file has no reader at all, so a missed exclusion costs nothing
    — which is why **a reader for `fe-inbox.jsonl` may not be introduced on such
    a platform without re-opening this question.**

Nothing about the transport changes.

### Amendment: a receipt only where a reader exists

**A receipt must mean some reader will see the frame. A frontend must not claim
a filing into a file no process on its operating system reads.** Off Windows no
process on the box ever opens `fe-inbox.jsonl` — the platform test belongs to
the readers, and `sot_fe_inbox_path` in `comm/core/scripts/comm-lib.sh` returns
success with empty output there, so every shell reader routed through it
resolves an empty path; the only Rust read is the `fe_down` baseline, which is
itself Windows-only. A claim from such a box was therefore a false success: the
sender was told `filed -> @h` for a message nothing would ever read.

**The append is deliberately not gated; only the claim is.** An append promises
nobody anything, a claim does. Two reasons keep the append: it is the only
per-process record that an inbound relayed frame reached the box at all — the
message arm logs nothing on success — and the only place to suppress it without
new machinery is the path-resolution failure arm, which already means "no state
dir". Routing a deliberate policy through a failure arm would make a genuinely
missing state dir indistinguishable from normal operation, and would log a
dropped-message warning for every message on two platforms. The two costs of the ungated append — the file grows with no reader, and on a
shared home several hosts' frontends append to one path under a lock — are
arguments about whether a frontend should file at all off Windows. That is a
delivery-architecture question, not an honesty question, and it is left open
here.

**The gate is a composition, not a branch inside the decision table.** The pure
function that reads the payload and the frontend's own-host rows keeps its whole
decision table and stays ungated; the caller the message arm uses applies the
platform test first and then defers to it. Folding the gate into the pure
function would make every negative case in its table pass off Windows for the
platform's reason instead of its own, leaving the id rule, the broadcast rule
and the not-in-the-set rule vacuous on the two legs that run most often. The
platform test is `cfg!(windows)`, not `#[cfg(windows)]`, for the same reason:
both arms compile everywhere, so one test asserts the correct answer on each leg
of the matrix.

**Exactly one route class changes verdict, from a lie to the truth:** a target
handle that is a row on a non-Windows frontend's own host, with no live relay
bridge, addressed from a box that cannot name it in its own registry. Its
verdict moves from `filed` to `NOT CONFIRMED`. **No class loses delivery.**
Cross-host claims were already impossible — a frontend resolves only rows of a
daemon that declared its own host — and where a bridge exists the bridge both
appends to the per-handle inbox and claims. In the gated class nothing was
arriving anyway.

**The sender's two negative answers are different branches, and this one lands
on the later of them.** `no such handle` is the empty-roster branch;
`NOT CONFIRMED: … nobody claimed it within 5s` is the branch where the roster
was non-empty, the ack carried an id, and no receipt arrived. The gated class
reaches the second because `receivers_for` in the backend counts any connection
whose `role` is `fe` as a receiver for every directed send, so the frontend's
own attachment keeps the roster non-empty. That rule is pinned by its own test
on every leg and is unchanged here.

**The price of the honest verdict is latency, and it is user-visible.** A send
in the gated class previously returned `filed` the moment the frontend's claim
came back; it now waits out the full 5 s bound and exits 1. Nothing on the ack
can shorten that — the roster is non-empty and the ack carries an id, so the
only thing that would end the wait early is a receipt, and there is correctly
none. A caller that treated a fast `filed` as the normal case will feel this as
a pause before a failure where it used to see an instant success. The pause is
the cost of not being lied to.

## Consequences

Verdicts, in the order the first that applies wins:

| what came back | verdict | exit |
|---|---|---|
| no ack, `ok != true`, or no `receivers` array | `ERROR: unreachable, nothing filed` | 1 |
| `--all` | `relayed -> <all> (N receiver(s))` | 0 |
| `receivers` empty | `no such handle: h` | 1 |
| ack without an `id` | `NOT CONFIRMED: this daemon predates filer receipts` | 1 |
| a receipt carrying this send's `id` | `filed -> @h (by <filer>, relay)` | 0 |
| no receipt before EOF or the 5 s bound | `NOT CONFIRMED: sent for @h; nobody claimed it within 5s. Attached: …` | 1 |

Everything a receipt cannot exist for is decided on the ack — a broadcast, a hub
that dropped the id, an empty roster — so no path waits out five seconds for an
answer nobody can give. **Two paths do wait.** The first: a frontend too old to
send `agent.filed` is indistinguishable from a slow one, so a send to a handle
such a frontend hosts costs the full 5 s before its NOT CONFIRMED. The second
(the amendment "a receipt only where a reader exists"): a handle hosted on a
platform whose inbox has no reader, where no receipt is coming by design — that
one also costs the full 5 s, and it is the honest answer rather than a gap to
close. The item-3 fix
this supersedes failed instantly there, but only because it matched the
name-suffix guess; without that guess, and without a negative claim, nothing on
the ack distinguishes an old frontend from a new one that is about to answer.
Closing it needs a capability signal the daemon can report — new wire surface,
and a separate decision.

The bridge sends its receipt over a fresh one-shot connection with the bridge
role cleared (no phantom second bridge in the roster, no long-lived-role read
deadline on a one-frame connection) and reads the ack with `grep -qm1`: the
daemon never closes a one-shot connection, so without that early exit every
filed frame would cost the full 5 s inside the bridge's filing loop, and a burst
would file at one message per five seconds.

**Mixed fleet.** No struct here uses `deny_unknown_fields`, and both new fields
are omitted when absent, so: a new sender against an old hub gets `predates
filer receipts` on the ack (no false success, no stall); an old sender against a
new hub publishes no id, nothing claims, and it behaves exactly as its own
version always did; a new hub with an older frontend as the filer appends the
frame as today and the sender reports NOT CONFIRMED naming it, after the 5 s
above; a new frontend against an old daemon has its `agent.filed` answered as an
unknown op, which the unmatched-id fallthrough drops silently (no per-frame
warn). Cross-Linux sends on a shared `$HOME` are registry hits and never touch
the wire at all.

**The return leg depends on the netcat flavor, not on the protocol.** The daemon
closes on read EOF, and only OpenBSD `nc` holds the write half open long enough
to carry a receipt back; a flavor that closes both halves would report NOT
CONFIRMED for every filed frame. Nothing hermetic can cover that — the smoke
test is a real send printing a real `filed -> … (by …)` line, and a receipt that
never arrives is the first thing to suspect.

**What this deletes.** The name-suffix guess and its "probably its filer"
reasoning; the roster-name success branch; `fe.files_for` on the wire; the "no
such handle" dead end for a target this box cannot name, and with it the
requirement that a caller know which verb routes; and the instruction that only
a reply proves the path.

**What it deliberately does not do.** It does not answer "is that remote handle
real". The earlier draft had a frontend answer `filed: false` for a handle no row
of its own declared, which would have closed remote liveness; decision 2a is why
it cannot. Liveness stays open. A `(handle, host)` address on every frame
would answer a question the receipt already answers better, and would let a
sender silently pick one of two boxes claiming a handle instead of reporting who
actually filed; if duplicate handles become real, the fix is the naming rule plus
two visible receipts. With no negative claim, a handle whose session has
simply not started yet reads as NOT CONFIRMED rather than as a refusal — the
frame IS still appended and a later session reads it, and the wording says the
frame was sent, not that the handle is unknown.

The amendment does not make the daemon the filer. The frontend is still the
filer because a daemon is not a standing client of another daemon: the
topology dial is a one-shot blocking CLI call, not a subscription, and nothing
in the backend holds an outbound connection over which an `agent.message`
fan-out could reach it — so the target box's daemon never sees the frame at
all. Moving the filing there is the change that would remove arbitration
entirely rather than perform it, and ADR 0047's closing paragraph already names
it as the next direction; it waits on that link. Nor does the amendment move
filing into the per-handle inbox, which is the change that would delete the
second inbox and its readers outright; its prerequisite is that the frontend
process learn the comm home and the handle charset, which it does not know.
