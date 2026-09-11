# ADR 0045: The lane bridge, and the protocol-versioned lane gate

**Status:** B1 (the gate) merged — e6c025c4 lands the proto-only gate,
f181d039 the same-day follow-up discharging a Codex review round as
shape fixes. B2–B5 in flight; B6 not started. Design pass 2026-09-11,
revised twice the same day after two Codex text rounds (see
`codex-design-verdict.md`/`-verdict2.md`), each finding cited at the
decision it fixed.
**Date:** 2026-09-11

Retires claims in three ADRs: ADR 0042 L3's shape ("proxy ops on the
control connection", "the same challenge runs end to end"), ADR 0041's
"Build boundary" and "Upgrade and version skew" (build-id equality as
the gate), and ADR 0043 decision 22 (no attach path on Linux "until the
bridge"). Adds one daemon op (`lane.connect`), deletes one gate (the
pre-spawn pair probe). ADR 0043's numbering is scoped to the Unix
capsule runtime; this ADR carries its own 1–11. Decision 33 of the
lifecycle track (`resume_if_absent`, ADR 0043 decisions 32–35) is the
recovery primitive decision 2 below calls.

## The decision in one paragraph

A frontend attaches to a capsule row **through the row's daemon**, on
every host including its own: a dedicated connection whose first frame
is `lane.connect` becomes, after one reply, a raw byte pipe onto the
supervisor lane or the voyage lane — the ADR 0035 `proxy.connect`
mechanism, verbatim, for lane endpoints instead of loopback ports. The
attach client's state machine is unchanged; it gets a second `Endpoint`
whose `Client` is that piped stream. The identity proof splits where
the OS forces the split: the daemon, whose kernel can see the lane's
peer, runs steps 1–3 on its own dial and reports the observed `(pid,
created)` in the reply; steps 4–5 (the wire `hello`) run end to end and
bind against that report — the frontend trusts the daemon it already
trusts to control the row, and nothing more. The daemon's dial is also
the recovery trigger: a supervisor lane found absent on a resumable row
is resumed before the dial is answered. The lane gates compare one
integer per wire contract; the build id stays on the wire as
information only; the pre-spawn pair probe is deleted because the hello
gate at first contact is the invariant it served; and the release that
ships the new gate is entered with every capsule row ended — one
migration mechanism, not two.

## Decisions

1. **One attach path: through the daemon, local or remote.** `pty.open`
   on a capsule row still answers `attach_direct`; the frontend then
   opens `lane.connect` connections to THAT row's daemon (its per-host
   `TransportConfig`), never a supervisor socket or a state-dir path
   directly. The frontend loses `PlatformEndpoint`, `state_dir_hash`,
   `sot_state_dir`, `pointer`, and every `#[cfg(windows)]` on the attach
   path — a Linux or macOS frontend attaches for free, at the cost of
   one in-box hop for a local row (the hop every WGL page already
   takes). Rejected: the daemon rendering and streaming `pty.evt`
   frames — a second resync mechanism the checkpoint already is.
   Invariant: one code path is the one place to get the identity right.

2. **`lane.connect` is a first-frame op on a dedicated connection, and
   the daemon's dial is the recovery trigger.** A connection beside
   `proxy.connect`, outside the hello-gated control loop (ADR 0035 §1).
   Request `{target, lane, voyage_id?, token?}`; `target` is the row's
   `tmux_session` name and is REQUIRED for both lanes — a voyage lane is
   reached only through the row that owns it. The daemon resolves the
   row → `unknown_workspace` / `not_capsule` / `unauthenticated`; for the
   SUPERVISOR lane, when the dial finds the endpoint absent on a row
   that is neither terminal nor starting, it calls the lifecycle track's
   `resume_if_absent` (ADR 0043 decision 33: resume-only, never `reset`,
   one launch in flight per row under the per-row guard) and dials
   again — a stale attach can never restart a destroyed row, and a dead
   authority over a live leg is re-established by the first client that
   asks; `lane_absent` is answered only when the resume fails or the row
   is terminal. For the VOYAGE lane an absent endpoint is `lane_absent`
   at once (the supervisor owns leg respawn). `authenticate_server`
   (steps 1–3) then answers `Authenticated` → pipe with `{ok, pid,
   created}`, `Foreign` → refuse and close, `Undetermined` → reported as
   itself, never as foreign. The pipe body is one `pipe_bidirectional
   (rx, tx, upstream)`; the daemon never decodes a lane frame after the
   reply. Invariant: recovery and identity share one dial.

3. **`DaemonLaneEndpoint` in `sot-protocol`.** `LaneDial::Tcp` (the
   loopback tunnel) or `LaneDial::Local` (a Unix socket or Windows named
   pipe). It holds NO row — the row rides the `Endpoint` calls
   (`connect_supervisor_unchallenged(&self, lane)`,
   `connect_voyage_unchallenged(&self, lane, voyage_id)`), so the target
   is named exactly once, never duplicated onto the endpoint value
   itself. Its `Client` is `DaemonLaneClient { stream, peer:
   PeerAuthenticated }`; `challenge()` runs the neutral
   `exchange_identity` (steps 4–5) and yields `Proven(BridgedPeer{pid,
   created})` only if the wire identity equals the daemon's own
   observation — property 22's binding, performed by the daemon.
   `BridgedPeer` implements ONLY `PeerIdentity{pid, created}` — no
   process-control methods for a peer this endpoint cannot itself wait
   on or terminate. Invariant: an endpoint value names one thing (a
   dial); a row is named in exactly one place.

4. **Refusals and uncertainty are typed, on every path.** `TransportError`
   gains three bridge-only variants: `Refused{code, detail}`
   (`unknown_workspace`, `not_capsule`, `unauthenticated`, `foreign`,
   `no_bridge` — an old daemon answering something other than a
   `lane.connect` result); `Unreachable(io::Error)` (dial/handshake
   failed); `Undetermined(detail)` (the daemon answered undetermined).
   Every connect site matches these BEFORE any io conversion: `Refused`
   is terminal with its code named in the pane line; `Unreachable`/
   `Undetermined` retry after backoff AND clear the unresponsive-since
   clock, so transport uncertainty is never charged to the absence
   window — an outage followed by a real absence starts a fresh one.
   Only `lane_absent` maps onto `Io{NotFound|ConnectionRefused}` and the
   120 s health window. Invariant: a hiccup and a genuinely dead row
   must never look alike to the client deciding whether to keep waiting.

5. **An `Endpoint` is a value; the row is named once.** The four trait
   methods take `&self`; `PipeEndpoint`/`SocketEndpoint` become unit
   values. `attach`/`attach_headless` take `(endpoint, lane, …)` in
   place of `state_dir`; `lane` is the row's name in the endpoint's own
   namespace (`h` for the platform endpoints, the row target for the
   daemon-lane endpoint), passed to both connects, including the
   supervisor's own voyage dials. Invariant: as decision 3 — one name
   for the row, never reconstructed twice.

6. **The attach client converges on the supervisor's word only.**
   Completes ADR 0043 decision 28: the three `pointer::validate` reads
   go — the post-Ready consistency check, the health path's pointer
   resolution, `TerminalReason::PointerAbsentOrCorrupt`. The pointer
   stays the supervisor's and the daemon's (`phase_of`); a client never
   reads it directly. Invariant: one authority speaks for a row's state;
   a file a client can also read is not that authority.

7. **The gate is the protocol integer.** The hello arm on both sides
   compares `proto` only; `hello{build}`/`hello_ok{build}` stay on the
   wire as information (ADR 0043 decision 31) and are never compared,
   and nothing about a peer's build is added to a refusal frame — it
   carries a reason, nothing else. An immutable fixture per proto
   (`supervisor-lane-v1.bin`, every request/reply variant, never
   regenerated) proves two processes agreeing on the integer agree on
   the BYTES; command bytes feed durable journal digests, so a lane bump
   changing a command's encoding is ALSO a `journal::SCHEMA_VERSION`
   event. `version.query` gains `lane_proto` beside the unchanged
   `lane_build`. Invariant: compatibility is one comparable integer per
   contract, not a string that names a build but proves nothing about
   what it can decode.

8. **The pre-spawn pair probe is deleted.** `check_pair` / `pair_verdict`
   / `PAIR_PROBE_BOUND` and the "rebuild the pair" text go — merged from
   the lifecycle track's L3. The invariant they served — the daemon can
   drive what it spawns — is now the hello gate at the daemon's first
   `status` after spawn: a proto skew is `foreign` on the row within one
   poll. `build-id` stays a diagnostic; the launcher's rename-aside
   compare is untouched. Invariant: one check does the job two did
   before — the second added no coverage the first-contact gate lacked.

9. **Adopting a supervisor of another build is ordinary.** The lane is
   the ONLY daemon-supervisor interface, so a daemon drives any
   supervisor speaking its `SUPERVISOR_PROTO`. Record integrity is
   already build-independent (the fence, `journal::SCHEMA_VERSION`,
   `record::WRAPPER_VERSION`, the ADR 0039 segment format and its
   goldens); no state-dir layout version is added — a value naming no
   invariant is a deletion candidate. `FOREIGN_PHASE`'s pane line names
   the recovery: end the row from a client of its proto, or kill only
   `supervise` and attach again — the dial resumes it. Invariant:
   decision 7's protocol is the only compatibility surface.

10. **One migration: the B1 release is entered with every capsule row
    ended.** A supervisor predating this decision refuses any client
    whose build sha differs; rather than a second mechanism for adopting
    old-gate authorities (kill-and-resume, previous-binary CI fixtures —
    both considered and deleted after the second Codex round), the
    converge that installs the release first drains every row with the
    OLD pair, then daemon, FE and capsule move together — already how FE
    boxes converge, and there are no backend capsule rows today. The
    release notes say it; every future lane bump owes the same note.
    Invariant: one upgrade mechanism, stated in the release notes, beats
    two silent ones.

11. **Outages are retries; only a genuinely absent supervisor is
    terminal.** The frontend's own `pty.open` re-fire on daemon
    reconnect is not the recovery path, and the lifecycle track deletes
    its steady-loop re-fire: recovery is the daemon's dial (decision 2),
    reached by the attach client's own episodes, so outstanding input
    survives inside the one client (`OutstandingSlot` resends the same
    key after re-attach). The FE's daemon connection and its lane
    connections die together; the episode ends on EOF; the next dial is
    `Unreachable` → retry, clock cleared, for as long as the outage
    lasts. The health window runs only on `lane_absent` — the daemon
    answered and could not resume. Recording never waits on any
    subscriber: one lagging by 4 MiB is dropped and re-attaches;
    checkpoint chunks are emitted one per step, so a slow link only
    lengthens the transfer. Invariant: a slow or interrupted link is
    never mistaken for a dead row, and recording never blocks on either.

## What this deletes

`pair_verdict`, `check_pair`, `PAIR_PROBE_BOUND`; build-string equality
in the supervisor's hello arm; the FE's `WorkspaceRuntime.state_dir`,
`attach_direct_state_dir`, `paths::sot_state_dir` for capsule use, and
its dozen `#[cfg(windows)]` attach-path gates; `pointer` resolution in
`fe_client_io.rs` and `TerminalReason::PointerAbsentOrCorrupt`; ADR
0042 L3's `attach.proxy`/`mgmt.proxy` ops and "same challenge end to
end" wording; ADR 0043 decision 22's "no attach path … until the
bridge" and its per-platform default (the flip is B6). Discharged from
earlier drafts by the two Codex rounds: an "unsupported → absent" map,
`BridgedPeer` process-control stubs, a kill-and-resume migration with
its previous-binary CI fixture, peer build display on a refusal, and
the reopened direct-dial question (decision 1 settles it). **Kept:**
`SUPERVISOR_LANE_BUILD_ID`, `build-id`, `version.query.lane_build`,
`FOREIGN_PHASE`, `Error::VersionSkew`, the attach lane's negotiation,
and — until the next `PROTOCOL_VERSION` bump —
`attach_direct.state_dir`/`workspace.list.state_dir` at zero cost.

## Lanes

| Lane | Scope | Status |
|------|-------|--------|
| B1 | The gate: proto-only comparison, `check_pair` deleted, `version.query.lane_proto` | Merged (e6c025c4, follow-up f181d039) |
| B2 | `Endpoint` by value, `PeerIdentity` split, unit endpoints, pointer deletions | In flight |
| B3 | `lane.connect` op + `lane_bridge.rs`; depends on the lifecycle track's L1a for `resume_if_absent` and the per-row guard | In flight |
| B4a | `DaemonLaneEndpoint`, the three `TransportError` variants | In flight |
| B4b | Cross-process proofs over a test-owned TCP→Unix relay (Linux job) | In flight |
| B5 | The frontend: `DaemonLaneEndpoint` wiring, `state_dir` reads deleted | In flight |
| B6 | The flip — after B5, B4b, and the lifecycle track's service-stop proofs | Not started |

Order: B1 → (B2 ∥ lifecycle L1a) → B3 → B4a → B4b → B5 → B6.
