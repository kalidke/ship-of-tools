# The project chat, route B: the Slack client in the daemon (plan, 2026-11-01)

Committed 2026-11-05 as the plan of record; the working copy it was taken from is the architect session's own. Absolute
paths are written relative to the repository root, and the maintainer's Slack workspace is named only as his.

Written by a read-only Codex planner for the architect session (a Codex job, exit 0),
against main 8cf5b2bed, from the maintainer's choice of route B (relayed by the captain, 2026-11-01 07:02 MST)
and the captain's list of five required items. Nothing was built and no Slack call was made. Checked by the architect
against the tree: the lock's 588 packages, the 512 MiB preview cap (`rust/protocol/src/ir.rs`), the in-process filer
entry `file_comm`, the admission test that reads the op module's source, and the shelved API module's 604 lines. The
new crates' versions, features, transitive counts and builds are unverified (none is in the local cargo cache).

**Correction to the comparison the choice was made on.** The architect's comparison put the Slack client at about
1,050 product / 1,200 test lines. This plan puts it (B1) at about 1,500 / 2,220: bounded raw responses, manual
redirects, conservative mutation outcomes, the shared fake Slack and TLS fixture, and Slack token masking in the
daemon's log were uncounted or undercounted. Whole-feature totals, handovers excluded:

| | Ship of Tools | SlackClaw | All |
|---|---:|---:|---:|
| Route A, the approved plan (five Ship of Tools landings) | 1,790 / 2,250 | 650 / 700 | 2,440 / 2,950 |
| Route B, this plan as first written (B1 to B5) | 3,670 / 5,180 | the pause switch only (pull request 18, already written) | 3,670 / 5,180 |
| Route B, revised below (13 preparations, then B1 to B5 chat only) | 4,420 / 6,730 | the same | 4,420 / 6,730 |

Part of the difference is not the route: the four landings both routes share (the hub service and wire, mail, figures,
the drawer) were re-estimated here at 2,170 / 2,960 against route A's 1,510 / 1,900 for the same landings. What route B
replaces directly (route A's chat child, 280 / 350, and SlackClaw's landing 1, 650 / 700, together 930 / 1,050) becomes
B1 at 1,500 / 2,220. The maintainer chose route B on the lower numbers; the captain's page should show these.

## Before the first piece: the chat’s place, and the preparations before each consumer

The section below was revised 2026-11-01 by a fourth read-only Codex pass (job
`dev/output/codex/jobs/chat-map-revise`, exit 0) to the answers recorded in "Review of the organization, and the
answers" further down; it supersedes the first map (kept in the architect's scratchpad). It corrected one fact in
answer 2: the filer reads the registry without the registry lock (`comm/mail/filer.rs` `file_here`), so P14 now takes
that lock in the filer. Open item for P14, from the architect: holding the registry lock across the inbox append couples
two locks, so before P14 lands its planner must show no path takes the inbox lock and then the registry lock (an order
inversion), and state the bound the hold adds to every registry writer; if either fails, P14 takes the lock for the
check alone and states the window between check and append as its limit.

Revised against main `9b7146655` to apply the seven answered findings. This section supersedes the earlier map and re-cut. The review’s decisions remain settled; the current-code correction needed for P14 is listed near the end.

There are thirteen preparations: P1–P11, P13 and P14. P12 is deleted. Each preparation belongs to the subsystem providing its guarantee and lands before its consuming chat landing, in the order specified below. Existing-pane recovery is outside this feature.

| Scope | Product / test lines |
|---|---:|
| Preparations | **1,125 / 2,130 — unverified estimate** |
| Chat-specific B1–B5 | **3,295 / 4,600 — unverified estimate** |
| Whole envelope | **4,420 / 6,730 — unverified estimate** |

All line allocations below are unverified estimates, excluding prose, manifests and the separately owned channel handovers. Transferred allocations are planning allocations, not measured implementation.

This revision was prepared by reading only. No files changed, builds or tests ran, product binaries started, credentials were inspected, or external services contacted.

### 1. The chat’s place in the package

`sot-chat` owns Slack and project-chat policy. Existing subsystems provide the native filesystem, logging, admission, transport and presentation mechanisms it needs. The binary composes them through narrow adapters.

**What `sot-chat` rests on**

| Dependency | Why it belongs below chat |
|---|---|
| `sot-platform` | Config/state locations, native locks, durable progress publication, safe managed-leaf operations and log masking. Chat supplies names and policy. |
| `sot-protocol` | The normalized record, operation payloads and status vocabulary. Wire owns representation; chat owns normalization and meaning. |
| Tokio | Tasks, permits, deadlines, streaming and cancellation. Chat owns its tasks and their retirement. |
| `serde`, `serde_json`, `toml` | Bounded Slack decoding and chat configuration. No copied topology parser. |
| `reqwest`, defaults disabled | HTTP requests, connection reuse and streamed bodies. Destinations, authorization, redirects and outcomes remain chat policy. |
| `tokio-tungstenite`, defaults disabled | Websocket framing and handshake. Chat owns acknowledgements, listener replacement, reconnect and gap state. |
| `rustls`, proposed ring provider; `webpki-roots` | The proposed shared TLS policy and trust roots. |
| `url` | URL parsing before chat applies destination and authorization rules. |
| `bytes`, `futures-util` | Bounded chunks and async stream/sink operations. |
| `tracing` | Fixed, sanitized diagnostics. Raw library errors and private URLs are not diagnostic inputs. |
| Test-only `tokio-rustls`, `tempfile`, platform test support | Loopback TLS fixtures and isolated state; fixed test certificates avoid another dependency. |

Exact networking versions, features, transitive closure, native build requirements and binary growth remain **unverified**. B1 must establish the compiler-only build evidence, including the Linux musl artifact route. Existing lockfile membership does not establish the daemon’s dependency cost.

Chat gains no dependency on topology, rows, messaging, files, pages, sidecars, lifecycle, containment or frontend code. No generic outbound HTTP crate is introduced: the updater’s curl-backed fetcher and pages’ incoming HTTP parser have different contracts.

**What rests on chat**

Only `sotd` links `sot-chat`. Its new `rust/backend/src/chat.rs` supplies these composition surfaces:

- Startup supplies a positively established hub fact, platform-resolved locations and configuration. Credentials open only after admission.
- Operations supply current canonical row facts and the expected workspace/channel pair, then return bounded typed results.
- Filing receives chat’s finished body and expected canonical row, constructs the generic filing request and returns the actual filer verdict unchanged.
- Media translates materialized paths into existing Files nodes and existing preview/docs requests.
- Events expose bounded changes and status; server retains ownership of connection writers.
- Stop closes admission, cancels chat’s tasks and reports retirement through lifecycle’s prepared surface.

The adapter does not classify origins, escape channel text, truncate mail, construct headers or choose attachment wording. Those are chat’s delivery policy.

The frontend and shell tools use wire operations. They obtain neither a Slack client nor credentials.

Strict hub admission uses topology’s current load/parser result and requires an explicit configured hub. `comm_topology_hub` accepts missing topology, and `TopologyStore::refresh` can retain an earlier valid topology after disappearance; neither shortcut supplies chat’s positive admission.

**Dependency direction**

| Caller | Dependencies introduced or reused |
|---|---|
| `sot-chat` | Wire, platform and the selected async/networking libraries |
| Binary chat adapter | Chat plus topology, rows, messaging and existing Files/pages surfaces |
| Server | Binary-built service context and chat handler/subscription adapters |
| Lifecycle | Its generic stop contract; no Slack or chat types |
| Fe-net | Wire chat requests, replies, events and generic transport outcomes |
| Fe-ui chat owner | Fe-net outcomes, shared selection and existing presentation/preview paths |
| Files, pages, sidecars | No dependency on chat and no chat read-hold capability |

Add `chat → wire, platform` to package.md’s target graph. The feature follows that target direction without claiming to remove the current backend/frontend cycles.

**One owner per concept**

| Concept | Owner |
|---|---|
| Config/state resolution, native locks and durable publication | Platform |
| Directory confinement and fixed-leaf create, rename and removal mechanisms | Platform |
| Log byte masking | Platform; server owns subscriber filtering |
| Topology grammar, configured hub and routing | Topology |
| Canonical row identity, inert-anchor predicate and handle binding | Rows |
| Slack connections, authorization, acknowledgements and rate limits | Chat |
| Project/channel mappings and mapping admission | Chat, using supplied row facts |
| Message identity, origin classification, normalization and reconciliation | Chat |
| Shared record representation and operation declarations | Wire |
| Mutation outcome semantics | Chat; fe-net reports transport loss |
| Inbox admission, expected-row check, liveness, append, cursor and fixed wake | Messaging |
| Marked mail body and durable channel recovery boundary | Chat |
| Attachment names, reservations, materialization, eviction and refetch | Chat |
| Watch registration and exclusions | Files |
| Files node identities and preview production | Files |
| Page confinement, grants and reply authority | Pages |
| Connection admission, dispatch, subscriptions and writer | Server |
| Generic event-loss signal | Server; representation in wire, decoding in fe-net |
| Connection lifetime, pending requests and unsupported-operation decoding | Fe-net |
| Shared flowed-text selection | Fe-ui’s shared text component |
| Chat history, threads, drafts, anchors and thumbnails | Fe-ui’s private chat owner |
| Row verbs and `show-result` | Agents |
| Messaging participation instructions | Messaging |
| Controlled exit and stop ordering | Lifecycle |
| Installer transformation and test discovery | Distribution |
| Current guarantees and user instructions | Records, amended by each provider |

Wire’s record is not another normalization implementation. Messaging’s expected-row check is not a claimant registry. Platform’s rename/unlink mechanism is not an eviction policy.

**Existing-file touch map**

P12 contributes no touches. New files are named separately after the records table.

| Existing file | Landing | Change and reason at this level |
|---|---|---|
| `rust/Cargo.toml` | B1 | Add the chat workspace member. |
| `rust/Cargo.lock` | B1 | Record the resolved dependency graph. |
| `rust/backend/Cargo.toml` | B2 | Link chat into its sole product caller. |
| `rust/backend/src/main.rs` | B2 | Declare the binary composition adapter. |
| `rust/platform/src/secret.rs` | P4 | Extend stateless masking at the shared log boundary. |
| `rust/platform/src/secret_tests.rs` | P4 | Exercise real formatter writes and retained page masking. |
| `rust/backend/src/daemon_log/mod.rs` | P4 | Suppress credential-bearing networking diagnostics independently of the general level. |
| `rust/backend/src/daemon_log/log_tests.rs` | P4 | Observe both sinks through the real subscriber. |
| `rust/platform/src/host/pinned_dir.rs` | P3 | Close directory-type/reparse gaps needed by the managed API. |
| `rust/platform/src/host/durable.rs` | P3 | Reuse native publication/flush mechanisms for confined fixed-leaf operations. |
| `rust/platform/src/host/mod.rs` | P3 | Export the managed-operation surface. |
| `rust/platform/Cargo.toml` | P3 | Declare required native bindings/features; exact additions remain unverified. |
| `rust/protocol/src/lib.rs` | P5; B1, B2 | Export generic loss vocabulary, then the shared record with B1 and operation payloads with B2. |
| `rust/protocol/src/ops/mod.rs` | P2, P5; B2 | Generate admission fixtures from the one declaration, add generic loss vocabulary, then chat declarations. |
| `rust/protocol/src/ops/agent.rs` | P14 | Add a defaulted optional expected canonical row to `CommFileReq`; generic filing representation belongs in wire. |
| `rust/backend/tests/admission.rs` | P2 | Remove source/count/membership assertions and consume generated fixtures in the real refusal/closure loop. |
| `rust/protocol/src/codec.rs` | P8 | Admit blobs against the caller’s bound before collection; retain one codec. |
| `rust/backend/src/server/mod.rs` | P7, P10, P11; B2 | Construct watcher inputs, context and stop participants once; B2 constructs/registers chat. |
| `rust/backend/src/server/listen.rs` | P10, P11; B2 | Carry the prepared context and stop surface; pass chat without another positional chain. |
| `rust/backend/src/server/conn.rs` | P5, P10; B2 | Preserve writer ownership; add generic loss handling and later connection-local chat subscriptions/cleanup. |
| `rust/backend/src/server/dispatch.rs` | P10; B2 | Select narrow handler arguments, then route chat requests off the connection loop. |
| `rust/backend/src/server/events.rs` | P5; B2 | Write generic loss status; B2 adds chat filtering/encoding. |
| `rust/backend/src/server/conn_select_once_wire_tests.rs` | P5 | Replace “lag writes nothing” with an observed loss frame. |
| `rust/backend/src/server/events_tests.rs` | P5; B2 | Prove generic loss first, then chat subscription routing. |
| `rust/backend/src/lifecycle/mod.rs` | P11 | Declare/export the narrow service-stop surface. |
| `rust/backend/src/lifecycle/shutdown.rs` | P11 | Invoke bounded service retirement through the existing controlled terminal. |
| `rust/backend/src/lifecycle/shutdown_tests.rs` | P11 | Observe ordering, incomplete retirement and unchanged outcome selection. |
| `rust/backend/src/rows/mod.rs` | P7 | Store generic exclusions beside existing watcher construction state. |
| `rust/backend/src/rows/registry.rs` | P7 | Pass exclusions to existing and future row watchers. |
| `rust/backend/src/files/watcher.rs` | P7 | Apply supplied exclusions to registrations and event ingress. |
| `rust/backend/src/files/watcher_tests.rs` | P7 | Observe registrations and events, including populated directory arrivals. |
| `rust/backend/src/comm/mail/filer.rs` | P14 | Acquire the existing registry lock for guarded filing, check the expected row on fresh bytes and retain the guard through append. No Slack policy. |
| `rust/backend/src/comm/mail/filer_comm_file_tests.rs` | P14 | Exercise moved bindings, successful guarded append, lock failure/release and legacy requests through the real filer. |
| `rust/backend/src/comm/mail/links.rs` | P14 | Preserve the expected row through offers and retain guarded move-refusal detail through aggregation; routing policy stays unchanged. |
| `rust/backend/src/comm/mail/links_tests.rs` | P14 | Observe the forwarded expectation and refusal; update ordinary request fixtures to omit the expectation. |
| `rust/backend/src/comm/cli/wire.rs` | P14 | Construct ordinary requests without an expected row; no new CLI flag or chat policy. |
| `rust/backend/tests/comm_file.rs` | P14 | Exercise the optional field through the admitted wire entry and observe no append on mismatch. |
| `rust/backend/tests/topology_set.rs` | P14 | Update the existing typed filing fixture and retain the forwarding control. |
| `rust/frontend/src/net/transport/mod.rs` | P9; B2 | Scope non-replayable work to one connection, then import chat payloads. |
| `rust/frontend/src/net/transport/request.rs` | P8, P9; B2 | Add bounded memory reads and connection-bound delivery before chat request variants. |
| `rust/frontend/src/net/transport/event.rs` | P5, P6, P8, P9; B2 | Define typed generic outcomes before chat outcomes; retain dial-host tagging. |
| `rust/frontend/src/net/transport/reply.rs` | P5, P6, P8, P9; B2 | Own matching, bounded reads and terminal transport outcomes, then register chat pending kinds. |
| `rust/frontend/src/net/transport/steady.rs` | P8, P9 | Prepare bounded reads and connection-scoped egress in the existing loop. |
| `rust/frontend/src/net/transport/ops/mod.rs` | P6; B2 | Own generic error classification, then declare the chat adapter family. |
| `rust/frontend/src/net/transport/ops/pages.rs` | P6, P9 | Use existing `page.reply` to prove unsupported and non-replayable outcomes before chat. |
| `rust/frontend/src/net/transport/ops/files.rs` | P8 | Add a bounded memory destination to the existing disk-download implementation. |
| `rust/frontend/src/net/transport/golden_tests.rs` | Preparations; B2 | Exercise encoded requests and decoded outcomes. |
| `rust/frontend/src/net/transport/testdata/every_kind.golden` | Preparations; B2 | Record observed wire shapes/outcomes. |
| `rust/frontend/src/ui/preview/shaped.rs` | P1 | Delegate generic selection; retain preview buffer, scroll, anchors and media policy. |
| `rust/frontend/src/ui/preview/shaped_tests.rs` | P1 | Preserve actual preview behavior alongside shared-component proofs. |
| `rust/frontend/src/ui/mod.rs` | P1, generic routing; B2, B5 | Declare owners; B2 introduces the minimal chat status/reply owner and B5 completes its view. |
| `rust/frontend/src/ui/init/fields.rs` | B2, B5 | Construct the private chat owner through its constructor. |
| `rust/frontend/src/ui/events.rs` | Generic routing; B2, B5 | Exhaustively route typed outcomes. B2 makes chat the first loss-signal consumer; P5 adds no existing-pane recovery. |
| `rust/frontend/src/ui/chrome/panes.rs` | B5 | Add Chat to the existing drawer vocabulary. |
| `rust/frontend/src/ui/input/layout_help.rs` | B5 | Preserve Help return origin, focus and drawer-toggle rules. |
| `rust/frontend/src/ui/input/keybindings.rs` | B5 | Add chat actions to the one action catalog. |
| `rust/frontend/src/ui/input/help.rs` | B5 | Admit chat actions and typed compose context through existing rules. |
| `rust/frontend/src/ui/input/help_drawer.rs` | B5 | Select the tenant’s help context. |
| `rust/frontend/src/ui/input/global_keys.rs` | B5 | Route Chat’s toggle and shared scrolling to its owner. |
| `rust/frontend/src/ui/input/mouse.rs` | B5 | Delegate chat hit testing/dragging while preserving presence/modal ordering. |
| `rust/frontend/src/ui/drawer/mod.rs` | B2, B5 | Declare the private chat owner. |
| `rust/frontend/src/ui/drawer/keys.rs` | B5 | Route Chat input before REPL handlers can consume it. |
| `rust/frontend/src/ui/chrome/draw.rs` | B5 | Supply chat projection and geometry to the frame. |
| `rust/frontend/src/ui/chrome/view.rs` | B5 | Reserve/title Chat’s space. |
| `rust/frontend/src/ui/render/pass/panes.rs` | B5 | Paint selection and thumbnails in the existing pass/clipping rules. |
| `rust/frontend/src/ui/app/frame.rs` | B5 | Prepare chat layout/text and paint within the existing frame sequence. |
| `rust/frontend/src/ui/session/switch.rs` | B5 | Notify chat of the selected canonical row without adding chat state to general snapshots. |
| `rust/frontend/src/ui/session/replies.rs` | Generic routing; B5 | Forward connection transitions and retirement effects to the owning model. |
| `rust/frontend/src/ui/session/workspace_list.rs` | B5 | Forward authoritative row retirement so drafts/views cannot acquire a replacement row. |
| `rust/frontend/src/ui/app/native_pane_tests.rs` | B5 | Register the chat workload in the existing native presentation route. |
| `agents/sot-fe/sot-fe` | B3, B4 | Dispatch the chat part and parse flags; daemon chat owns channel policy. |
| `agents/claude/bin/show-result` | B4 | Execute/report show and post independently, preserving HTML Queue offers. |
| `agents/tests/test-show-result.sh` | B4 | Observe both requested operations, including show failure and unknown publication. |
| `test/install_tests.jl` | P13; B3 | Replace the fixed part count with installed execution; B3 exercises the installed chat verb. |
| `comm/adapters/claude/sot-comm/SKILL.md` | B3 | Describe marked channel mail, project authority limits and in-thread replies. |
| `comm/adapters/codex/skills/sot-comm/SKILL.md` | B3 | State the same participation contract with the existing no-idle-wake limitation. |
| `agents/claude/show-result/SKILL.md` | B4 | Describe explicit publication and separate outcomes. |
| `comm/legacy-skills.txt` | B3, B4 | Regenerate retirement hashes when shipped skill bodies change. |

These existing surfaces need no product edit:

| Surface | Reason |
|---|---|
| `rust/backend/src/files/mod.rs`, `transfer.rs`, `preview/mod.rs` | P12 is deleted. Existing readers receive no chat lease. |
| `rust/backend/src/pages/mod.rs`, `site/mod.rs`; sidecars | P12 is deleted. No response-lifetime or worker-lifetime hold plumbing. |
| `rust/backend/src/rows/handle.rs`, `anchor.rs` | Reuse `bound_handle` and `is_inert_default_anchor`. |
| `rust/backend/src/comm/registry/registry.rs`, `lock.rs` | Reuse the existing registry representation and lock mechanism; P14 acquires that lock in the filer. |
| `rust/topology/src/lib.rs`, `rust/backend/src/topology/store.rs` | Reuse current load/parser surfaces without changing fallback semantics. |
| `rust/frontend/src/net/transport/ops/preview.rs` | Existing preview requests suffice after Files node admission. |
| `src/comm_bin.jl`, `comm/bin-folders.txt` | Part embedding already exists and `agents/sot-fe` is already listed. |
| `.github/test-shards.txt`, `scripts/tests/shards.py` | Existing workspace discovery supplies initial coverage; placement changes require measured need. |
| `scripts/tests/rc-gate.sh`, `under-bash.sh` | Existing discovery includes workspace tests and shell suites. |
| `.github/workflows/rust.yml`, `release-gate.yml` | Existing builds and native route carry the feature; no speculative workflow edit. |

**Contracts and records**

| Record | Landing | Required amendment and reason |
|---|---|---|
| `CLAUDE.md` | B1, B2 | Import new folder contracts when added; import lines only. |
| `docs/package.md` | Preparations; B1–B5 | Add chat’s guarantee/dependencies and only the guarantees each landing establishes. |
| `docs/integration.md` | Each changed connection | Provider-owned descriptions of managed operations, loss notification, stop, guarded filing and chat adapters. Delete the proposed read-hold connections. |
| `rust/platform/CLAUDE.md` | P3, P4 | State confined managed-leaf operations and masking, including native limits and held-read deletion proof. |
| `rust/protocol/CLAUDE.md` | P2, P5, P8, P14; B1, B2 | State generated fixtures, loss vocabulary, bounded blob admission, defaulted filing expectation, B1 record representation and B2 operations. |
| `rust/protocol/src/ops/CLAUDE.md` | No duplicate policy | Retain its parent-contract link. |
| `rust/backend/src/server/CLAUDE.md` | P5, P10, P11; B2 | Describe context/stop connections and observable loss; explicitly retain the limit that existing panes do not recover automatically. Then name chat dispatch/subscriptions. |
| `rust/backend/src/lifecycle/CLAUDE.md` | P11 | Amend the controlled-terminal sequence for bounded service retirement. |
| `rust/backend/src/rows/CLAUDE.md` | P7; B3 | Describe generic watcher input and the adapter’s reuse of existing row facts. |
| `rust/backend/src/comm/CLAUDE.md` | P14; B3 | Add guarded filing admission under the registry lock; later name the in-process chat caller. Formatting remains outside messaging. |
| `rust/backend/src/comm/mail/CLAUDE.md` | No duplicate guarantee | Retain the messaging parent-contract link. |
| `rust/backend/tests/CLAUDE.md` | P2, P14 | Keep the parent link; any added suite description names behavior rather than a source inventory. |
| `rust/topology/CLAUDE.md` | B2 | Name reuse of the current load result; no grammar/routing change. |
| `rust/backend/src/files/CLAUDE.md` | P7 | State supplied managed-root exclusions. No read-hold promise. |
| `rust/backend/src/pages/CLAUDE.md`, sidecars’ contract | No P12 amendment | Existing readers and grants retain their contracts. B4 reuses them. |
| `rust/frontend/src/net/CLAUDE.md` | P5, P6, P8, P9; B2 | State generic transport guarantees before chat requests/events. |
| `rust/frontend/src/ui/CLAUDE.md` | P1, generic routing; B2, B5 | Move selection ownership; describe the private chat owner and its lifecycle/routing. |
| `rust/frontend/src/ui/input/CLAUDE.md` | B5 | Extend selection/input routing to Chat. |
| Drawer, preview and render folder contracts | No duplicate guarantee | Retain parent links; the new chat folder gets its own contract. |
| `agents/CLAUDE.md` | P13; B3, B4 | Describe installed-part behavior, row verbs and separate show/post outcomes. |
| `scripts/CLAUDE.md` | P13; B1 evidence | Correct installer-test wording and record build evidence without adding credential provisioning. |
| `comm/PROTOCOL.md` | P14; B3 | Document the optional expected row and its admission rule; channel mail remains a marked body in the existing inbox format. |
| `requirements.md` | B2 | Permit external channel participation while preserving account and approval boundaries. |
| `docs/src/ref/config.md` | B2 | Document configuration, hub admission, restart semantics and the storage bound. |
| `docs/src/guide/figures.md` | B4 | Describe explicit publication, materialization and eviction/refetch behavior. |
| `docs/src/guide/previews.md` | P1; B4 | Describe shared selection and existing attachment-open routes, including an evicted file’s explicit failure/refetch. |
| `docs/src/guide/interface.md` | B5 | Describe Chat history, thread and compose behavior. |
| `docs/src/ref/keybindings.md` | B5 | Describe actions from the existing catalog. |

New files are:

- P1’s shared selection component and tests.
- P2’s declaration tests; P5’s generic status payload/tests.
- P3’s managed-directory module, native arms and tests.
- P10’s `server/context.rs` and tests; P11’s lifecycle stop surface and tests.
- P14’s filing payload compatibility tests, alongside the agent family.
- B1’s chat crate, contract and fixtures, plus `rust/protocol/src/chat.rs` and its record tests.
- B2’s binary adapter, `protocol/src/ops/chat.rs`, fe-net chat adapter and minimal private UI chat owner/contract.
- B3’s chat shell part and suite.
- B5’s remaining private chat presentation/model modules and native workload.
- `docs/src/guide/chat.md`, established with the first usable chat surface and completed with the drawer.

### 2. Preparatory landings and their proofs

Each preparation amends its provider’s contract and integration row. Tests observe production behavior; no source scan proves a guarantee. Missing APIs may initially expose an executable refusal. Compilation failure alone is not the behavioral red.

| Landing / owner | Rule and files | Product / test lines |
|---|---|---:|
| **P1 — Shared flowed-text selection / fe-ui** | Two consumers copy identical UTF-8 ranges and paint equivalent clipped geometry. Soft wraps add no newline; media reservations add no text. New `ui/flowed_selection.rs` and tests; `ui/mod.rs`, `preview/shaped.rs`, `shaped_tests.rs`. | **90 / 130 — unverified estimate** |
| **P2 — Operation fixture / wire and server** | Wire’s single declaration emits admission fixtures; every declared request is refused and closed before hello. `protocol/src/ops/mod.rs`, declaration tests, `backend/tests/admission.rs`. | **25 / 90 — unverified estimate** |
| **P3 — Managed directories / platform** | Fixed-leaf create, publication, rename and removal stay within held directories despite substitution; publication never replaces an existing target. Successful eviction preserves an already-open reader’s bytes. New managed/native modules and tests; pins, durable operations, exports and manifest. | **320 / 560 — unverified estimate** |
| **P4 — Credential-safe logging / platform and server** | Complete daemon events contain no admitted credential bytes in either sink and retain page masking. General logging cannot re-enable unsafe networking diagnostics. Secret and daemon-log modules/tests. | **80 / 150 — unverified estimate** |
| **P5 — Generic bus-loss signal / server, wire and fe-net** | Lag produces typed resync status through the existing writer and typed decoding in fe-net. Chat first consumes it in B2. Status payloads/declarations, server event/connection tests, transport decoding/goldens and necessary exhaustive routing only. | **60 / 110 — unverified estimate** |
| **P6 — Unsupported operations / fe-net** | An old daemon’s actual unknown-operation reply terminates only that request as unsupported. Existing `page.reply` is the initial proof. Transport ops/reply/event/pages tests, goldens and outcome routing. | **50 / 100 — unverified estimate** |
| **P7 — Managed watch exclusions / files** | Supplied roots consume no descendant registrations and emit no preview events, including populated directory arrivals; siblings remain observable. Watcher/tests, rows construction and server input. | **70 / 150 — unverified estimate** |
| **P8 — Bounded memory reads / wire and fe-net** | Memory downloads enforce the requested bound before collection, validate chunks/completion and emit one terminal result. Codec/tests and existing file-download transport paths. Chat’s B4 thumbnail path is the first product consumer. | **120 / 200 — unverified estimate** |
| **P9 — Connection-scoped mutations / fe-net** | A non-replayable request admitted for one connection cannot transmit on a later connection; an unanswered possible submission becomes unknown once. Transport lifetime/queue/reply paths and existing `page.reply` proof. | **80 / 160 — unverified estimate** |
| **P10 — Binary service context / server** | Connections share constructed service handles; handlers receive narrow arguments without parallel positional lists. Build on existing `Shared`; new context/tests plus construction/listen/conn/dispatch. | **60 / 80 — unverified estimate** |
| **P11 — In-process retirement / lifecycle** | Controlled serving-daemon exits close registered admission and request stop before termination; retirement completes within the terminal budget or reports incomplete. Stop surface/tests, shutdown and server integration. | **80 / 140 — unverified estimate** |
| **P13 — Installed part execution / distribution and agents** | An additional valid sourced part becomes executable within the installed tool, without separate installation or a fixed part count. `test/install_tests.jl` and contract wording; no installer product change. | **0 / 60 — unverified estimate** |
| **P14 — Expected-row filing / messaging, representation in wire** | A request may name its expected canonical row. The appending filer checks fresh registry binding/liveness under the registry lock and retains the guard through append. A moved binding refuses `not_here` naming the move and appends nothing. Filer, payload, route/constructor fixtures and owner/wire tests listed above. | **90 / 200 — unverified estimate** |

P12 is removed, not deferred as another prerequisite.

**Behavioral proofs and contract amendments**

| Landing | Behavioral proof | Contract paragraph and value |
|---|---|---|
| P1 | Shape wrapped Unicode in the shared fixture and preview; exercise drag direction, resize, scroll, outside press, media reservations and replacement. | Fe-ui’s shared-owner and `ShapedPreview` selection paragraphs. Chat motivates sharing; preview retains its behavior. |
| P2 | Run real first-frame refusal/closure over generated fixtures plus unknown/malformed frames, invalid hello and declared-blob controls. | Wire’s op ownership and server guarantee 4; remove the admission-scan exception. New declarations automatically join coverage. |
| P3 | Native substitution fixtures observe outside sentinels; test existing-target/regular-file refusal, failed publication and cleanup. On each system, hold a real reader open across rename/unlink and compare the complete original bytes; a later open fails not-found. | Platform guarantees 2 and 10 plus the managed-operation paragraph. The mechanism is generic; chat decides eligibility and refetch. |
| P4 | Real tracing formatting with multiple fields, padding, synthetic credentials/page secrets, both sinks and elevated filtering. | Platform guarantee 14 and daemon subscriber wording. Retain the documented separate-writer-call limitation. |
| P5 | Overflow a real small receiver, drive the connection writer and decode the status in fe-net. A failed status write ends the connection. B2 separately proves chat’s response. | Replace server guarantee 16’s silent notification limit with an observable loss signal; fe-net promises typed delivery. Existing-pane recovery remains outside the guarantee. |
| P6 | An old-daemon fixture returns the current unknown-op shape for `page.reply`, then answers another request on the same connection. Malformed success remains a separate error. | Fe-net’s typed outcome promise. |
| P7 | Observe production registration calls and real events for excluded roots and positive siblings, including directories populated before notification. | Files’ watcher paragraph and rows’ construction connection. |
| P8 | Serve valid, oversized, discontinuous, truncated and malformed streams; observe rejection before oversized collection and exactly one terminal outcome. Preserve disk-download controls. | Wire’s blob admission and fe-net’s download paragraphs. |
| P9 | Disconnect before dequeue, during partial write and after acceptance without an answer; reconnect and count actual requests. | Fe-net’s reconnect/pending-request paragraphs. No surviving mutation executes after reconnect. |
| P10 | Distinguishable fixture handles reach the correct handlers across accepted connections. Retain admission, off-loop and writer proofs. | Server guarantee 14 and construction paragraph. |
| P11 | Participants record admission closure, stop, completion and terminal ordering. Stalled participants/runtime produce explicit incomplete retirement without changing the chosen exit outcome. | Lifecycle guarantee 6 and server’s shutdown connection. Chat is the first new participant; existing daemon tasks are not migrated for this feature. |
| P13 | Install a synthetic tool with an extra sourced part, execute its action from the isolated install and observe that no separate part was installed. | Agents guarantee 13 and distribution’s installer-test description. |
| P14 | Hold guarded filing before admission, move the registry binding through its normal writer, release filing and observe `not_here` plus unchanged inbox bytes. Also prove matching-row success, absent expectation, stale/missing/malformed entries, lock failure, guard release and preservation through forwarding/offers. | Messaging’s Sending paragraph, wire’s additive-payload paragraph and `comm/PROTOCOL.md`’s filing request. Stable targeting becomes an explicit generic admission guarantee. |

**P3’s native boundary**

On Unix, the managed API walks from held descriptors and operates on fixed single-component leaves using exclusive creation, descriptor-relative publication, rename and unlink. It never recursively removes a caller-supplied tree.

The Windows candidate remains root-handle-relative native opens with directory verification and reparse refusal, followed by handle-based rename/disposition. Exact bindings and supported behavior remain **unverified** until native tests run. A pathname check followed by a write is not sufficient.

The held-read proof must cover the reader forms used by the actual attachment routes, including the kernel-mediated path where applicable. Proving only chat’s writer share mode is insufficient: Windows readers must permit delete sharing for the promised successful rename/unlink. The existing directory pin deliberately withholding delete sharing is a different object and guarantee.

P3 establishes the native mechanism. It does not strengthen every legacy pathname caller or claim Windows parent-directory deletion durability.

**P14’s filing admission**

The optional wire field is defaulted so an ordinary request without an expectation retains existing behavior. B3 supplies the canonical row from rows’ current facts to the updated in-process filer. Forwarding and offers preserve the expectation; it is never silently downgraded to ordinary filing.

For a guarded local append, messaging acquires its existing registry lock, freshly reads the entry, checks that `workspace_id` still matches the expected canonical row, applies existing presence/liveness rules and keeps the guard until the existing appender returns. Lock acquisition/read failure is an explicit refusal. A mismatch is `not_here` with the move named and no append.

The lock is newly acquired by this path; it is not currently held there. Checking under the lock and releasing it before append would retain the race. The implementation must use bounded acquisition and release on success, failure and unwind, without holding the registry lock across network forwarding.

This changes neither inbox format nor cursor/wake policy. It introduces no claimant registry, private liveness algorithm or Slack-aware messaging field.

**P5’s first consumer and the existing-pane notes**

P5 ends at the generic loss signal and its transport decoding. B2’s minimal chat owner is its first product consumer: it marks the watched state incomplete and initiates the existing chat snapshot/reconciliation path or displays an explicit gap if recovery cannot complete. It never replays a mutation.

The following remain notes for their owners’ queues, with no product edits or line allocations in this feature:

| Owner | Recovery note |
|---|---|
| Rows / workspace presentation | Refresh authoritative workspace state after loss. |
| Topology / its frontend consumer | Refresh topology after loss. |
| Files / preview presentation | Refetch affected preview state after loss. |
| Monitor / its frontend consumer | Refresh available monitor state and identify unavailable intervals. |
| Sidecars / REPL presentation | Mark lost output or run-state information explicitly; never reconstruct output or replay commands. |

These are code-reading notes, not defects observed in use. Server’s contract continues to state the existing consumers’ recovery limit.

**Other reuse and deletion decisions**

- Shared bounded HTTP collection stays in chat. Transfers reuse its counting/deadline machinery while streaming.
- Sanitized errors, sensitive headers and private-URL suppression remain B1 rules; P4 is the final log boundary.
- Chat constructs the bounded marked mail body. The adapter only translates facts, request and verdict.
- Existing Files node conversion, preview/result admission and scope-`none` docs serving remain the attachment routes. No new preview/docs protocol is needed.
- P7 receives exclusions before watcher construction, including future rows. Chat owns `.sot-chat` naming; Files applies supplied roots. Configuration still takes effect on restart.
- The existing drawer vocabulary, action catalog and frame adapters gain a Chat case. REPL internals do not become chat infrastructure.
- Existing part embedding and test discovery remain sufficient.
- No chat child, Julia environment, extra connection route, durable outgoing queue, parent catalog or chat sequence protocol is added.

### 3. Landing order

The agreed early group is **P2, P4, P10 and P11 before B1 and B2**, with P10 before P11.

The remaining preparations precede their first chat consumers:

| Consumer | Preparations required before it |
|---|---|
| B1 | The agreed early group; B1 does not require P3. |
| B2 | P5, P6 and P9, in addition to the early group |
| B3 | P14 |
| B4 | P3, P7 and P8 |
| B5 | P1 |
| Independent test correction | P13 may land at any point. |

Preserve these shared-surface orders:

- P2 precedes P5’s status declaration and B2’s chat declarations.
- P10 precedes its context consumers, including P5, P7 and P11.
- P6 → P9 → P8 settles classification, connection lifetime and bounded reads in order.
- Shared transport/UI event-router edits serialize.
- Provider contracts and integration rows merge in landing order.

Chat remains **B1 → B2 → B3 → B4 → B5 → separately owned channel handovers**. The post-tag condition remains. There is no universal preparation barrier before B1.

### 4. Re-cut B1–B5

| Landing | Chat-specific contents | Product / test lines |
|---|---|---:|
| **B1 — Native Slack client and shared record** | Chat facade, HTTP/API/transfer/normalization/socket modules, safe errors and loopback TLS fixtures; crate/lock/contracts. Land the one normalized record in wire and return it directly. No temporary chat-owned record or conversion layer. | **1,465 / 2,130 — unverified estimate** |
| **B2 — Hub service, operations and adapters** | Configuration/service, positive hub admission, binary row-fact/operation adapter, wire requests/events/declarations, server subscriptions and typed fe-net family. Introduce the minimal private chat status/reply owner and first loss-signal consumption. Reuse B1’s record. | **530 / 740 — unverified estimate** |
| **B3 — Delivery and recovery** | Origin filtering, bounded marked-body construction, delivery/progress and complete traversal in chat; adapter translates the body and expected row into P14’s filing request and returns its verdict. Row post/reply verbs and participation instructions. | **420 / 620 — unverified estimate** |
| **B4 — Media and publication** | Reservations, materialization, closed-leaf rename/unlink eviction and file-ID refetch; existing media-route adapters; fetch/post-file verbs; separate show/post execution and outcomes. Uses P3/P7/P8, with no reader holds. | **380 / 520 — unverified estimate** |
| **B5 — Drawer and native witness** | History/thread model, compose, anchor-preserving paging, thumbnails and shared-selection consumer; tenant/input/help/frame/lifecycle adapters and native workload. Uses P1 and existing result/preview paths. | **500 / 590 — unverified estimate** |
| **Chat-specific subtotal** | | **3,295 / 4,600 — unverified estimate** |

The B1/B2 boundary transfers **45 / 60 product/test lines — unverified estimate** for record representation and its tests from B2 into B1; it adds no second record implementation. B4 is re-estimated for the replacement eviction/refetch proof.

**B4’s replacement for P12**

Chat may evict only a completed leaf whose producing writer/transfer is closed. It renames that leaf to a generated eviction name within the managed directory, then unlinks it through P3. Active partials/transfers remain protected by chat’s reservation state.

An already-open reader retains its original bytes across successful eviction. A reader that has not opened the leaf gets not-found through the existing route’s explicit error, and chat can refetch by file ID under F4. A docs-open acknowledgement or queued kernel request does not count as an open reader.

No lease passes through Files, pages or sidecars. Failed rename/removal is reported and remains accounted for by chat’s storage owner until cleanup succeeds.

Amend F1’s proof to protect active writers/transfers without requiring reader holds. Extend F4’s proof to:

1. Open and hold a real reader.
2. Evict through chat’s production media owner.
3. Finish reading and compare every original byte.
4. Observe a later open fail not-found.
5. Refetch by file ID and serve the bytes again, or expose the actual Slack unavailability.

Run that integration witness on every supported system. P3 separately proves the native operation; B4 proves eligibility, accounting and refetch policy.

**Totals and retained completion evidence**

Preparations total **1,125 product / 2,130 test lines — unverified estimate**. Together with B1–B5, the envelope is **4,420 / 6,730 — unverified estimate**. This is a scope estimate, not evidence that implementation fits those lines.

B1 retains HTTPS/websocket, normalization, uncertainty, streaming and acknowledgement proofs. B2 retains mapping, admitted-hello, off-loop responsiveness, old-daemon, loss-consumption and existing-link proofs. B3 retains real filer/poll/wake, moved-binding refusal and old-thread recovery proofs. B4 retains byte equality, reservations, Git-ignore, watcher, native substitution, eviction/refetch, remote preview and over-cap docs proofs. B5 retains the real 200-message/10-image native presentation witness. Live Slack gates remain unverified with their stated stop consequences.

The historical inventory is `dev/output/briefs/lanes-066/scan-inventory.md`. P2 removes R28’s admission scan. P14 now touches R01’s historical filer surface: the current tree already has behavioral filer tests rather than that scan, so preserve and extend those behavior tests. The old claim that R01 is irrelevant because the filer is unchanged is deleted. P13 replaces the fixed part-count assertion. Every final write set must be checked against the inventory; no landing adds or broadens a source-text behavioral proof.

**Current-code correction to the answered review**

- Answer 2 says the filer already reads liveness under the registry lock. It does not: [`file_here`]( rust/backend/src/comm/mail/filer.rs:249) reads the registry without that lock, then calls the inbox appender. The existing registry lock helper is separate in [`registry.rs`](rust/backend/src/comm/registry/registry.rs:218). The smallest correction is the P14 acquisition described above, held across the fresh binding/liveness check and append.

**Touches moved since the last version, and why**

- Delete P12 and its Files/pages/server read-hold plumbing. Eviction policy and integration proof move to B4; native rename/unlink and held-reader proof belong to P3.
- Restore the filer touch under P14 and add the filing payload, constructor/route fixtures, behavioral tests and contracts. Stable targeting requires an expected-row check at append admission.
- Move mail formatting from the binary adapter into chat’s B3 delivery owner. The adapter supplies facts and translates calls.
- Move normalized-record representation and tests from B2 to wire with B1, before its first producer returns it.
- Narrow P5 to loss vocabulary, writer notification and decoding. Chat consumes it in B2; existing-pane recovery becomes owner notes.
- Move each remaining preparation to its agreed consumer boundary. P1 no longer blocks B1, and P3/P7/P8 precede B4.
- Keep installer-list, speculative preview-adapter and workflow edits deleted because existing surfaces already carry the feature.
## Review of the organization, and the answers (added 2026-11-01)

A third read-only Codex pass (job `dev/output/codex/jobs/chat-map-review`, exit 0, main 9b7146655) reviewed the map
and P1 to P13 adversarially against the maintainer's standard. Its verdict: subsystem placement and most reuse decisions
are sound, but the plan does not yet meet the standard until findings 1 to 4 are fixed; finding 5 and the ordering
barrier can be reduced without weakening the done test. Its review follows whole, then the architect's answer to each
finding. The answers changed the plan: the section "Before the first piece" above was revised to them the same day,
with its lines re-estimated.

### Answers

1. **Read holds follow the caller, but reads can outlive it (blocker). Accepted; P12 is replaced.** A hold around the
   handler cannot cover a kernel read or a blocking read that outlives its awaiting task. Rather than move holds into
   sidecars' workers, delete P12 and make eviction safe without holds: chat evicts only a closed, completed leaf by
   rename and then unlink; a reader that already opened it keeps its bytes (Unix unlink semantics; on Windows the
   managed leaves are opened with delete sharing, which P3's native proof must show); a reader that arrives after
   eviction gets not-found, an explicit failure, and chat refetches by file id (rule F4). Files, pages and sidecars
   carry nothing for chat. Tests: an eviction during a held read on each system returns the whole original bytes; a
   read after eviction fails not-found and a refetch serves the file again. This deletes a preparation, three
   providers' capability plumbing and a cross-provider type.
2. **Resolving the handle before filing does not keep the target row (should-fix). Accepted; a messaging preparation
   is added (P14).** The filer checks presence and liveness but not which row holds the handle, and a binding can move
   while a file waits. P14 (messaging, chat-motivated): a filing request may name the row it expects; the filer checks,
   under the registry lock where it already reads liveness, that the handle's entry still binds that row, and fails
   `not_here` naming the move otherwise. The binary adapter passes the canonical row from rows' current facts; no
   claimant registry, no private liveness check. Test: a binding moved while a filing is held is refused and nothing
   is appended.
3. **The adapter is given mail formatting policy (should-fix). Accepted.** `sot-chat` builds the bounded, marked body
   (origin header, escaping, truncation, attachment wording); the adapter supplies admitted row facts, makes the
   existing filing request from chat's body and returns the filer's verdict unchanged.
4. **The shared record lands after its first producer (should-fix). Accepted.** B1 lands the one normalized chat record
   in wire and returns it directly; B2 adds only the operation declarations, requests and events. No temporary record,
   no conversion layer.
5. **P5 bundles chat's loss signal with repairs to every pane (should-fix). Accepted.** P5 narrows to the generic half:
   the loss vocabulary in wire, the connection writer's resync frame in the server, its decoding in fe-net, with chat
   as its first consumer. Recovery of the existing panes (workspace, topology, preview, monitor, the REPL's runs) is
   not done for chat: server guarantee 16 states the drop as its limit and no stale pane has been seen in use, so each
   goes to its owner's queue as a note.
6. **The universal barrier (note). Accepted.** Each preparation lands before the first chat landing that consumes it,
   keeping the shared-file orders (P6, then P9, then P8; P10 before its context consumers): P2, P4, P10 and P11 before
   B1 and B2; P5, P6 and P9 before B2; P14 before B3; P3, P7 and P8 before B4; P1 before B5. P13 any time.
7. **The claims and the direction hold (note).** Recorded. Its second row, the in-process filer's sufficiency for
   stable targeting, is answered by P14.

Answered by the architect session, 2026-11-01. None of P1 to P14 is a defect of 0.7.0 (P9's premise is real, but only a
v0.7.0-rc.1 daemon can reach the window's reply bar, and none runs; the bar's deletion is queued for fe-ui's next pass).

### The review, whole

Reviewed main `9b7146655` by reading only. No builds, tests, product commands, network, mail or credential files were used.

1. **Read holds follow the caller, but reads can outlive it — blocker**

   **Concerns:** P12 and the claim that B4 needs no further subsystem preparation.

   **Evidence:** [P12](dev/output/briefs/slack-client-plan.md:289) promises protection through the final source read, including kernel previews, while also releasing holds when requests end. Those lifetimes differ in current code. [Files preview](rust/backend/src/files/preview/mod.rs:442) awaits `Kernel::request`; [kernel submission](rust/backend/src/sidecars/kernel.rs:285) can leave accepted work running after the caller times out. The supervisor retains that work independently. Files also starts blocking reads whose execution can survive cancellation of their awaiting task.

   A hold surrounding the handler future therefore permits eviction while a worker still reads. The proposed cancellation test could pass while this invariant fails.

   **Change:** Transfer the opaque hold to the actual worker. Kernel completion ownership needs a narrow sidecars surface, or P12 must explicitly restrict its supported path so no kernel reader can outlive the hold. Reserve and test that owning change before claiming B4 contains only chat work. Sidecars needs no Slack knowledge.

2. **Resolving the handle immediately before filing does not preserve the target row — should-fix**

   **Concerns:** B3’s row/filer adapter and the assertion that existing surfaces completely supply delivery admission.

   **Evidence:** [The plan](dev/output/briefs/slack-client-plan.md:695) resolves a joined row and handle immediately before calling `file_comm`. But [handle movement](rust/backend/src/rows/handle.rs:138) can transfer that handle to another row. [The filer](rust/backend/src/comm/mail/filer.rs:249) checks registry presence and liveness, then appends; it does not validate the intended canonical row. [CommFileReq](rust/protocol/src/ops/agent.rs:26) carries no row identity.

   Thus a binding move while filing waits can deliver the old project’s message to the handle’s new owner. This is a design gap, not an observed failure.

   **Change:** Define the filing admission point and preserve the row/binding relationship through it using a Rows-owned capability or fence. Exercise a binding move while filing is held. Retain messaging’s existing liveness and append implementation; add no claimant registry or private liveness check.

3. **The binary adapter is assigned chat formatting policy — should-fix**

   **Concerns:** The promise that `backend/src/chat.rs` is only a translation layer.

   **Evidence:** [The adapter description](dev/output/briefs/slack-client-plan.md:322) explicitly makes it construct the bounded marked mail body. That body’s origin header, escaping, truncation and attachment-failure wording are channel-delivery policy, which the concept table assigns to Chat. The existing filer accepts complete text; it needs no Slack-aware formatter.

   **Change:** Chat produces the bounded, marked body. The binary adapter supplies admitted row facts, translates it into the existing filing request and returns the actual verdict. Keep all origin classification and message formatting in `sot-chat`.

4. **The shared record lands after its first producer — should-fix**

   **Concerns:** B1 normalization, B2 wire types and their landing order.

   **Evidence:** [The re-cut](dev/output/briefs/slack-client-plan.md:354) puts normalization and the client facade in B1, but the chat wire family in B2. Meanwhile, wire owns the shared record’s representation and Chat owns normalization. The touch map reserves protocol changes for P2 and B2, leaving B1’s public normalized result type unspecified.

   This invites a temporary chat-owned record, a second serialized record with conversion code, or an unreserved protocol edit in B1.

   **Change:** Land the common normalized record in wire with B1, and make B1 return it directly. Leave operation declarations, requests and events in B2. Specify this boundary before implementation; no additional generic record framework is needed.

5. **P5 bundles chat’s loss signal with repairs to every existing pane — should-fix**

   **Concerns:** Preparatory scope and spread.

   **Evidence:** [P5’s proof](dev/output/briefs/slack-client-plan.md:300) includes workspace, topology, preview and monitor refetching, plus visible loss handling for REPL output and commands. Current [server event writers](rust/backend/src/server/events.rs:119) do log and drop lag; that premise is true.

   The proposed repair nevertheless changes several existing product behaviors beyond chat’s done test. REPL routing and completion already have a substantial private owner, described in [the UI contract](rust/frontend/src/ui/CLAUDE.md:378). “Existing consumer adapters” does not provide the requested file-level reservation for this work.

   **Change:** Keep the generic loss vocabulary and connection-writer mechanism with server/wire/fe-net. Narrow the chat prerequisite to its required consumer. Either defer existing-pane recovery to those owners’ queues or give it separate, explicit landings, write sets and estimates. Chat needs observable loss; it does not require repairing every existing lossy stream first.

6. **Most preparations have the right owner; their motivation and ordering need tightening — note**

   **Concerns:** P1–P13 and the universal “all before B1” barrier.

   | Landing | Assessment |
   |---|---|
   | P1 | Correct fe-ui extraction. Chat motivates sharing; an independent fixture alone is not independent product value. |
   | P2 | Keep. Wire generates the fixture; server observes admission. This removes an actual source scan. |
   | P3 | Correct platform owner. Keep the native guarantee narrow and its unverified Windows mechanism explicit. |
   | P4 | Correct split between platform masking and server filtering; honestly chat-motivated. |
   | P5 | Narrow or split as finding 5 describes. |
   | P6 | Correct fe-net compatibility change. |
   | P7 | Correct Files mechanism, carried through Rows’ existing watcher construction. Chat supplies names. |
   | P8 | Correct wire/fe-net change; identify Chat as the first memory-download consumer. |
   | P9 | Correct fe-net lifetime change. Distinct from P6; no reason to merge their guarantees. |
   | P10 | Keep. It addresses positional copies already identified by server’s contract; build on existing `Shared`. |
   | P11 | Correct lifecycle owner and explicit first-consumer motivation. Keep a narrow bounded stop surface; avoid an unsolicited migration of existing daemon tasks. |
   | P12 | Incomplete until worker lifetime is addressed. |
   | P13 | Keep the behavioral replacement. No installer product change is needed. |

   **Evidence:** The plan already names different first consumers: P1 serves B5; P7/P8/P12 serve B4. Their completion is not a dependency of the native networking client.

   **Change:** Keep preparations before their consuming chat landings, while preserving the stated shared-file ordering. Delete the universal prerequisite barrier unless there is a separate release reason for it. Retain P6 → P9 → P8 and P10 before its context consumers.

7. **The principal current-code premises and dependency direction hold — note**

   **Concerns:** Claims checked against the current tree.

   | Claim | Result and evidence |
   |---|---|
   | Missing topology makes `comm_topology_hub` true | **True:** [filer.rs:160](rust/backend/src/comm/mail/filer.rs:160). Unsuitable for strict chat admission. |
   | `TopologyStore` can retain topology after disappearance | **True:** [store.rs:111](rust/backend/src/topology/store.rs:111). Current `load_in` is available separately. |
   | Rows already owns handle binding and the inert-anchor predicate | **True:** [handle.rs:66](rust/backend/src/rows/handle.rs:66), [anchor.rs:17](rust/backend/src/rows/anchor.rs:17). Reuse them. |
   | An in-process filer already exists and checks liveness | **True:** [filer.rs:95](rust/backend/src/comm/mail/filer.rs:95). Its sufficiency for stable row targeting is **false** without finding 2’s admission rule. |
   | Admission coverage currently reads op source | **True:** [admission.rs:117](rust/backend/tests/admission.rs:117). |
   | Frontend outgoing requests survive reconnects | **True:** [transport/mod.rs:193](rust/frontend/src/net/transport/mod.rs:193). P9 has a real premise. |
   | Unknown operations use the stated textual error shape | **True:** [dispatch.rs:341](rust/backend/src/server/dispatch.rs:341). |
   | The codec collects blobs without a caller’s total bound | **True:** [codec.rs:122](rust/protocol/src/codec.rs:122). The initial reservation bound is not a total-size bound. |
   | Existing frontend downloads target disk | **True:** [reply.rs:129](rust/frontend/src/net/transport/reply.rs:129). |
   | Rows constructs existing and future watchers | **True:** [registry.rs:53](rust/backend/src/rows/registry.rs:53). |
   | Current Windows pins lack the proposed descendant-open guarantee | **True:** [platform contract:72](rust/platform/CLAUDE.md:72). |
   | Existing masking recognizes page secrets rather than Slack token forms | **True:** [secret.rs:8](rust/platform/src/secret.rs:8). |
   | Files already converts paths to node IDs | **True, with limits:** [tree.rs:108](rust/backend/src/files/tree.rs:108) performs path conversion, not a physical-confinement proof. |
   | Ordinary docs can carry an in-root attachment with scope `none` | **True:** [pages/ops.rs:229](rust/backend/src/pages/ops.rs:229). Preserve its existing confinement. |
   | The preview binary cap is 512 MiB | **True:** [ir.rs:19](rust/protocol/src/ir.rs:19). |
   | Installer part embedding already exists; its test fixes the count at four | **True:** [comm_bin.jl:125](src/comm_bin.jl:125), [install_tests.jl:269](test/install_tests.jl:269). Delete the proposed extra installation lists. |
   | New workspace libraries receive existing test discovery | **True:** [shards.py:117](scripts/tests/shards.py:117). No speculative routing list is needed. |
   | The lock has 588 packages and lacks the proposed networking stack | **True:** [Cargo.lock](rust/Cargo.lock). Dependency/build costs remain unverified. |

   The proposed dependency direction is acyclic and agrees with [package.md’s target](docs/package.md:283): Chat depends on wire/platform, while the binary composes service capabilities. Server, fe-net, Files, pages, lifecycle, Rows and messaging need no Slack policy. Keep P12’s capability types from introducing an unlisted dependency between those providers.

The plan does **not yet meet the maintainability standard**. Its subsystem placement and most reuse decisions are sound. Approval should wait for worker-owned read holds, explicit row-filing admission, removal of mail policy from the adapter, and a corrected record/landing boundary. The broad P5 repair and universal preparation barrier can be reduced without weakening chat’s done test.## The plan as first written (route B, before the map)

| Landing | Product lines | Test lines | Crates added | Done test |
|---|---:|---:|---|---|
| **B1 — Rust Slack client** | ~1,500 | ~2,220 | New `sot-chat`; proposed `reqwest`, `tokio-tungstenite`, `rustls`, `webpki-roots`; `tokio-rustls` as a test dependency. New transitive counts **unverified**. | The real client passes loopback HTTPS/websocket scenarios, including lost mutation responses, redirect authorization, streaming and acknowledgement under load. |
| **B2 — Hub service and wire** | ~570 | ~780 | Existing workspace crates only | A mapped hub row can watch, page and post through its existing daemon connection; other daemons read no credentials. |
| **B3 — Mail, recovery and row verbs** | ~420 | ~620 | None | With the window closed, a channel post becomes marked mail, wakes a free Claude row and receives an in-thread reply; incomplete recovery never advances progress. |
| **B4 — Figures and managed storage** | ~560 | ~800 | Existing platform dependencies only | Original figure bytes round-trip; inbound images open through remote preview; a file beyond preview capability opens through ordinary docs serving. |
| **B5 — Drawer and native proof** | ~620 | ~760 | None | Channel history, threads, compose, selection and thumbnails work; the native 200-message/10-image scrolling witness passes. |
| **H0 — SlackClaw pause switch, PR 18** | Existing patch: **529 additions / 194 removals** | Existing patch: **597 additions** | No Ship of Tools dependency | The old fleet stops admitting work for the moving channel while other channels continue; actual outcomes are accounted for. |
| **H1 — KALClaw channel handover** | ~90 | ~180 | None planned | Retired primary, listen, scheduled and proactive routes stay retired across restart; every handover witness is accounted for. |
| **H2 — LLClaw channel handover** | ~90 | ~180 | None planned | The same accounting holds across its deployment/restart path; an incomplete drain cannot report success. |
| **Ship of Tools total** | **~3,670** | **~5,180** | One subsystem crate; external dependency graph unresolved | All six items of the maintainer’s done test pass. |

These are planning estimates of maintained Rust and shell code, excluding prose and manifests. H0 is an existing shelved change, not additional route B implementation. H1–H2 retain the approved plan’s estimates; their current repository implementations were not re-read in this task and those estimates remain **unverified**.

The plan is against Ship of Tools **`8cf5b2bed`**, confirmed locally, version `0.7.0-rc.8-dev`. This review changed nothing, built nothing, ran no product binary and used no network or messaging service. Implementation starts only on the maintainer’s word, and landings into main follow the `0.7.0` tag.

**Evidence and the corrected comparison**

The binding scope is the supplied `sot-chat-drawer.md` brief and the complete `dev/output/briefs/chat-drawer-plan.md`. Repository ownership and landing rules come from `CLAUDE.md`, `requirements.md` and `docs/package.md`, especially “Draft contracts,” “Depends on” and “Order of passes.”

The comparison’s **1,050 product / 1,200 test lines is too small for the stated guarantees**:

| Client part | Comparison | Revised estimate | Reason |
|---|---:|---:|---|
| HTTPS API, history, posting, names and files | 650 product | **820 product / 980 tests** | Includes bounded raw responses, query-bound pagination, manual redirects, file membership, streamed upload stages, source-change checks and conservative mutation outcomes. |
| Socket Mode | 250 product | **330 product / 450 tests** | Includes authentication, one connection owner, acknowledgement before dispatch, bounded close, reconnect, duplicate-app checks and a persistent gap indication. |
| Message normalization | 150 product | **220 product / 350 tests** | Includes edits, deletes, supported subtypes, exact timestamps, recoverable origin, file stubs and encoded-size bounds. |
| Library interface and shared fake Slack | Uncounted | **50 product / 290 tests** | The public facade, shared HTTP/websocket fixture and TLS fixture are necessary test surfaces. |
| **Native client subtotal** | **1,050 / 1,200** | **1,420 / 2,070** | Excludes mapping, recovery policy, mail, storage and UI. |
| Platform masking and daemon log integration | Uncounted | **80 product / 150 tests** | Existing masking recognizes page secrets, not Slack tokens. |
| **B1 subtotal** | — | **1,500 / 2,220** | — |

These are estimates, not a measured Rust implementation. Their local cross-check is the shelved Julia work:

| Local branch read | Relevant source present | Test file present |
|---|---:|---:|
| `origin/feat/chat-api`, PR 17, `d3e058743` | `chat_api.jl`: 604 lines | 631 lines |
| `origin/feat/chat-files`, PR 19, `02b912337` | API 712 + files 293 | 1,200 lines |
| `origin/feat/chat-child`, PR 20, `b7c572ef3` | API 713 + files 303 + child 810 | 2,138 lines |
| `origin/feat/chat-docs`, PR 21, `16f014824` | Those three modules remain; setup support and documentation are added | 2,167 lines |

The branches are cumulative; their counts must not be added together. They establish implemented handling and fixture expectations, not successful live Slack access. Rust deletes Julia-version compatibility and child framing, but typed records, TLS setup and bounded native transports still need code.

The current workspace has no locked HTTPS/TLS/websocket client stack. `rust/updater/src/fetch/mod.rs` delegates HTTPS to `curl`; existing page servers provide local HTTP serving, not a reusable Slack client.

**Subsystem and ownership**

Add **chat as its own crate from the start**, `rust/chat`, package name `sot-chat`. Keeping it inside sidecars would give process supervision ownership of an in-process external service. Keeping it inside messaging would give inbox delivery ownership of Slack, media and history. Neither matches their contracts.

Proposed paragraph for `docs/package.md`:

> **chat.** The crate `sot-chat` owns the daemon’s project-channel mappings, Slack connections, normalized chat records, attachment store and delivery progress. It gives callers bounded history and file operations, explicit connection and recovery status, and a definite or unknown outcome for each submitted mutation; an unknown mutation is never resubmitted automatically. Only the configured hub reads chat credentials, and one owner holds each configured workspace listener. A recovery boundary advances only after its complete exposed traversal has been filed successfully. Rests on: wire, platform, Tokio and the selected HTTPS/websocket libraries. Owns: `chat.toml`, the reading of `chat-secrets.toml`, per-channel progress under daemon state, and `.sot-chat/` contents. Does not: own row identity, append inboxes, type into terminals, supervise children, grant browser authority or hold window state. Slack retention, deleted content, API access and the proven recovery traversal limit its recovery guarantee.

The crate depends on **wire and platform**, with no dependency on the backend binary, rows, messaging, files, pages or sidecars. The daemon supplies narrow capabilities:

| Concept | Owner and boundary |
|---|---|
| Hub identity | **Topology** supplies the decision from its existing parser; chat adds no host grammar or discovery route. |
| Canonical row, inert anchor and bound handle | **Rows** supplies current facts through daemon adapters. |
| Slack transport, mapping, origin and reconciliation | **Chat** owns them. |
| Inbox append, liveness and wake | **Messaging** retains its existing filer and fixed wake. |
| Config/state locations, native directory operations and log masking | **Platform** retains these mechanisms. |
| Admission, dispatch and one connection writer | **Server** routes chat operations; it implements no Slack behavior. |
| Preview node admission and watcher exclusion | **Files** owns these integrations. |
| Preview/result installation and browser grants | **Preview/pages** retain their existing paths. |
| Typed connection requests and unknown results on disconnect | **Fe-net** owns transport behavior. |
| Drawer state, input, selection and presentation | **Fe-ui** owns them. |
| Row verbs, `show-result` and shipped instructions | **Agents/messaging**, according to their existing split. |
| Daemon shutdown | **Lifecycle** supplies stop notification; chat cancels and joins its own tasks. |
| Packaging and test registration | **Distribution** carries the compiled client; it provisions no credentials. |

This keeps the new dependency direction acyclic. It requires no sidecar or containment contract change.

**What survives route A**

| Approved-plan landing | Route B treatment |
|---|---|
| SlackClaw additive API/child release | **Falls away as a Ship of Tools prerequisite.** The shelved work supplies API evidence and the Julia live harness. |
| Supervised chat child | **Deleted from the design.** No child readiness, heartbeat, stdin configuration, stdout framing or replacement owner. |
| Installed Julia chat environment | **Deleted from the design.** No `julia/chat`, SlackClaw package pin, installer preparation or child resource stamping. |
| Hub chat owner and wire | **Becomes B2**, constructed directly over the Rust client. |
| Channel mail, progress and row verbs | **Survives as B3**, with direct in-process calls. |
| Figures | **Survives as B4**; Rust streams files directly. |
| Drawer, records and selection | **Survive as B5**, with the corrections below. |
| Claude/Codex participation instructions | **Survive**, without child-specific language. |
| KALClaw/LLClaw retirement | **Survive as H1–H2.** PR 18 is needed only for this handover. |

The new order is **B1 → B2 → B3 → B4 → B5 → channel handovers**. Live gates can be completed independently through SlackClaw’s session and delivered through the captain. There is no cross-repository release order for the Rust client.

**Dependencies and build conditions**

`rust/Cargo.lock` contains **588 package entries**. Walking its recorded dependency edges gives the following existing closures. These counts exclude the named root and include the lockfile’s combined feature, target and development edges; they are not per-platform release counts.

| Existing dependency reused | Locked version | Recorded descendants | New lock entries from reuse |
|---|---|---:|---:|
| Tokio | 1.52.2 | 21 | 0 |
| serde | 1.0.228 | 6 | 0 |
| serde_json | 1.0.149 | 13 | 0 |
| TOML | 1.1.2+spec-1.1.0 | 15 | 0 |
| futures-util | 0.3.32 | 10 | 0 |
| bytes | 1.11.1 | 0 | 0 |
| URL | 2.5.8 | 34 | 0 |

The backend-rooted lock graph currently reaches **232 other package versions**. URL is absent from that graph: making it a daemon dependency makes **26 existing lockfile versions newly reachable there**, including IDNA/ICU components. “Already in the workspace lock” therefore does not mean “free in the daemon.”

Local manifests and relevant sources were read under `~/.cargo/registry/src/`. The proposed new networking crates, including the rejected TLS alternatives, were absent from that source cache and their registry metadata was unavailable locally. Accordingly, exact versions, selected feature spellings, new transitive counts and native build requirements are **unverified**, not estimated.

| Addition | Why needed; what it replaces | Build and transitive status |
|---|---|---|
| `sot-chat` | Enforces the new subsystem boundary; replaces the proposed backend chat module plus child-facing adapter. | Ordinary workspace Rust crate. External closure depends on the resolution below. |
| `reqwest`, defaults disabled | HTTP request construction, JSON calls and streamed bodies; replaces HTTP.jl and avoids hand-building an HTTP client. | Select Rustls, JSON and streaming only. Exact feature set, Linux/macOS/Windows compiler-only build and transitive count **unverified**. |
| `tokio-tungstenite`, defaults disabled | Async Socket Mode websocket framing and handshake; replaces HTTP.jl WebSockets. | Select client connection/handshake and the shared Rustls configuration. Exact features, build matrix and transitive count **unverified**. |
| `rustls`, explicit ring provider | One TLS policy for HTTP and websocket clients; replaces the child’s TLS dependency. | Defaults must not select aws-lc. Compiler-only support on all three systems, including the Linux release target, **unverified** locally. |
| `webpki-roots` | Supplies the product trust roots without platform certificate discovery. | Source, resolved closure and compiler-only build **unverified** locally. |
| `tokio-rustls`, development dependency | Serves the fake Slack over real TLS. | Expected to share the product TLS stack; that sharing and its incremental count are **unverified**. |
| Existing `url`, `bytes`, futures utilities | URL validation, streaming chunks and websocket sink/stream operations. | No new lock versions from promotion. Any additional enabled features must be counted after resolution. |

Do not add `tokio-util` merely for `ReaderStream`: existing Tokio file reads plus a futures stream can produce bounded chunks. Do not add a separate HTTP server framework or runtime certificate generator for the fake.

The rejected alternatives are:

- **Hyper plus Rustls directly:** rejected initially because connection management and body plumbing would become chat-owned product code; reconsider only if the resolved reqwest graph violates the box rule.
- **Another websocket client:** rejected because it would introduce another async/TLS integration; `tokio-tungstenite` fits the existing Tokio runtime. No locally verified comparison of alternative crate closures is claimed.
- **aws-lc-rs:** rejected for this plan because its native build requirements have not been shown to meet the no-cmake rule; ring is the proposed provider, subject to the same actual build proof.
- **System OpenSSL/native-tls:** rejected because it would add a system TLS prerequisite.
- **Platform certificate verification:** rejected initially in favor of one reproducible root policy; corporate interception or a required private CA would reopen this choice.
- **A subprocess running curl:** rejected because route B calls for an in-process client and token-bearing subprocess input would add another lifecycle boundary.

Before B1 lands, its dependency record must contain exact versions, enabled features, deduplicated new transitive counts for each supported target, and the selected native build requirements. It must prove builds with the normal Rust toolchain and C compiler, **without cmake or system OpenSSL**, including the existing Linux musl artifact route.

A defensible daemon binary-size estimate cannot be obtained from the absent crate sources and unresolved feature graph. Record size growth from matched release builds once authorized; do not substitute a guessed figure.

**Files and line allocation**

New module names below are proposed. Existing paths name the owners read during this review.

| Landing and file/module | Product | Tests |
|---|---:|---:|
| B1 `rust/chat/src/lib.rs` | 50 | 50 |
| B1 `rust/chat/src/http.rs` | 180 | 250 |
| B1 `rust/chat/src/api.rs` | 260 | 300 |
| B1 `rust/chat/src/transfer.rs` | 380 | 430 |
| B1 `rust/chat/src/normalize.rs` | 220 | 350 |
| B1 `rust/chat/src/socket.rs` | 330 | 450 |
| B1 shared fake Slack and TLS fixtures | 0 | 240 |
| B1 platform `secret.rs` extension | 60 | 100 |
| B1 daemon log filtering/integration | 20 | 50 |
| **B1 total** | **1,500** | **2,220** |
| B2 chat `config.rs` | 100 | 130 |
| B2 chat `service.rs` | 120 | 160 |
| B2 backend `src/chat.rs` adapters | 80 | 110 |
| B2 protocol `src/ops/chat.rs` | 100 | 120 |
| B2 protocol op declaration/admission fixture | 25 | 20 |
| B2 server construction, dispatch, watches and lifecycle glue | 65 | 120 |
| B2 fe-net request/reply/event support | 80 | 120 |
| **B2 total** | **570** | **780** |
| B3 chat `delivery.rs` | 150 | 200 |
| B3 chat `progress.rs` | 110 | 170 |
| B3 backend row/filer adapters | 40 | 80 |
| B3 `agents/sot-fe/sot-fe-chat.sh` | 100 | 120 |
| B3 CLI entry and installation-list wiring | 20 | 50 |
| **B3 total** | **420** | **620** |
| B4 chat `media.rs` | 260 | 320 |
| B4 platform managed-directory/leaf operations | 150 | 240 |
| B4 files watcher exclusion | 10 | 70 |
| B4 backend fetch/preview adapters | 40 | 60 |
| B4 `agents/claude/bin/show-result` | 70 | 90 |
| B4 existing file/preview request adapters | 30 | 20 |
| **B4 total** | **560** | **800** |
| B5 drawer chat model | 200 | 250 |
| B5 presentation | 120 | 100 |
| B5 reply adapters | 60 | 90 |
| B5 shared flowed-text selection extraction | 90 | 130 |
| B5 input, events, drawer and render adapters | 80 | 70 |
| B5 native chat fixture | 70 | 120 |
| **B5 total** | **620** | **760** |

B1 also adds `rust/chat/Cargo.toml`, its contract and workspace membership, and changes `rust/Cargo.lock`. Tests live beside the owning modules, with shared test-only support.

B2 touches the server’s `mod.rs`, `listen.rs`, `conn.rs`, `dispatch.rs` and `events.rs`; lifecycle shutdown integration; protocol op declarations; and fe-net’s `request.rs`, `event.rs`, `reply.rs`, `steady.rs` and new `ops/chat.rs`.

B5 adds `ui/drawer/chat/` and changes drawer selection, `input/layout_help.rs`, key/mouse/help routing, `events.rs`, initialization and render adapters. Selection moves out of `preview/shaped.rs` into a component owned by fe-ui and used by both consumers.

Each landing updates its owning contracts and connection descriptions in the same commit. Records include `docs/package.md`, `docs/integration.md`, requirements, a chat guide and configuration reference. The requirements amendment permits external channel members to direct the project row while preserving the single OS-account environment. New folder contracts receive the required root-map import in the same landing.

**Configuration, credentials and the network boundary**

Chat reads two files, both under platform’s existing config resolver:

- `chat.toml`: canonical project-folder mappings, the one workspace's ID and its channel IDs, optional `mail_after`, and the single attachment-storage bound **B**.
- `chat-secrets.toml`: the workspace ID and its bot/app-token pair.

On Unix, platform selects `XDG_CONFIG_HOME/sot`, otherwise `HOME/.config/sot`. On Windows it selects the existing `LOCALAPPDATA\sot\config` location, with platform’s existing profile fallback. Chat invents no path resolver or token environment variable.

Only a daemon positively identified as the configured hub opens the secret file. Missing or invalid topology leaves chat inactive. Missing chat configuration gives an actionable explanation; malformed configuration enables no delivery.

Tokens remain in the daemon’s config file and daemon memory. They enter no row environment, argv, repository artifact, frontend payload, mail or diagnostics. There is no child to receive them. Unix provisioning uses owner-only permissions; Windows retains the profile protection described by platform’s current contract. Rows share the daemon’s OS account: this feature adds **no OS security boundary** preventing a session of that account from reading the file. The prohibition on session token-file reads remains binding.

`Secret` values have redacted formatting. Transport failures become fixed error codes and bounded local explanations, never formatted request objects or library errors. Authorization headers are marked sensitive. Private file and websocket URLs remain internal. Network-library diagnostic targets are suppressed in the daemon subscriber even when its general log level is raised.

Platform’s existing `RedactingWriter` currently masks `secret=` values and exact 32-character lowercase hexadecimal runs. Extend its byte recognizer to admitted Slack token forms while preserving page masking. Keep platform stateless; do not register token values in a global platform store.

The cached tracing formatter, `tracing-subscriber-0.3.23/src/fmt/fmt_layer.rs`, formats an event into one buffer before calling its writer. The new behavioral log proof must exercise that real formatting path. The existing limitation for a secret manually split across separate writer calls remains documented; chat diagnostics must not use such writes.

Automatic redirects are disabled. Each private-download redirect is resolved and validated before another request:

- HTTPS only, no userinfo, no unexpected port or malformed authority.
- Bot authorization is rebuilt only for `slack.com` or a dot-boundary subdomain.
- Relative redirects preserve that rule.
- Redirect count and response bytes are bounded.
- Initial product destinations are restricted to the Slack hosts supported by the live witnesses. A newly observed non-Slack CDN destination fails visibly until its narrowly defined unauthenticated access is reviewed.

The same validated destination policy applies to websocket and upload URLs. Upload-byte requests carry **no bot authorization**.

Live in the maintainer's workspace (2026-11-01): every private download observed (a 217-byte PNG original, a phone JPEG's 992,922-byte original and its `thumb_360`) was a single hop to `files.slack.com`, HTTP 200, with the authorization sent and no redirect. The manual redirect policy stays as the guard; nothing observed asks for a destination amendment.

**Rules and behavioral tests**

Every numbered rule below has a named test. These are planned proofs; none ran during this read-only task. A compilation failure alone is not a behavioral red. New entries first expose an executable unavailable/refused facade, then exercise the real implementation through the same fixture.

| Rule | Test and observation |
|---|---|
| **C1. Only the configured hub reads credentials, from platform’s config folder.** | `only_hub_reads_chat_credentials_from_platform_config` — synthetic config homes and an observing loader show no secret-file open on a non-hub, including a shared-home case. |
| **C2. A credential cannot escape through formatting, client errors, wire output or mail.** | `chat_secrets_stay_inside_daemon_boundaries` — sentinel credentials are injected into malformed responses and transport errors; recording sinks contain neither token nor private URL. |
| **C3. Complete daemon log events mask Slack credentials while retaining page-secret masking.** | `daemon_log_masks_slack_and_page_secrets` — the real subscriber writes an event assembled from multiple fields; file and stdout captures contain masks and no sentinels. |
| **C4. TLS verifies certificates and names, and every redirect independently controls authorization.** | `private_redirect_keeps_slack_auth_and_refuses_foreign_destinations` — a Slack-host redirect receives authorization; lookalike, foreign, downgraded and malformed destinations receive none; invalid certificates fail before HTTP service. |
| **C5. An operation has one owner-held deadline and bounded admission and buffers.** | `chat_operations_keep_bounds_after_caller_drop` — stalled headers/bodies and a dropped caller cannot release the operation’s permit prematurely; excess work returns busy and observed buffers stay bounded. |
| **C6. History continuations retain their exact workspace, channel, thread and bounds.** | `history_pages_exact_query_without_silent_completion` — scripted channel/thread pages preserve exact timestamps; altered queries, repeated/missing continuations and malformed pages fail explicitly. |
| **C7. A post is submitted once, and a missing or ambiguous acknowledgement is unknown.** | `lost_post_answer_is_unknown_and_never_resent` — the fake accepts the body then closes; its request count remains one across caller cancellation and reconnect. |
| **C8. Rate limits use the response retry header without resubmitting mutations.** | `retry_header_paces_reads_and_preserves_mutation_draft` — mixed-case headers control the next read eligibility; a mutation is not repeated. Invalid/missing headers produce an explicit conservative limited state. |
| **C9. Upload streams original bytes and completes sharing at most once after source checks.** | `upload_streams_bytes_and_checks_before_single_completion` — allocation, incremental bytes and completion are observed separately; source change or cancellation prevents completion; a lost completion answer is unknown. |
| **C10. File fetch verifies channel sharing and enforces its reservation despite missing or false size hints.** | `private_fetch_checks_membership_size_and_response_kind` — wrong-channel metadata, oversized streams, absent variants and unexpected sign-in HTML produce named failures with no published partial. |
| **C11. Each Socket Mode envelope is acknowledged before event work begins.** | `socket_ack_precedes_slow_api_and_dispatch` — the server withholds further progress until it sees the correct acknowledgement; blocked name/file lookups cannot delay it. |
| **C12. A workspace has one active socket and replacement waits for the previous socket’s retirement.** | `disconnect_refresh_and_silent_close_keep_one_socket` — warning, refresh, disconnect and silent-close scripts never observe overlapping connections; shutdown prevents another connect. |
| **C13. Queue overflow or failed event handling leaves a visible reconciliation requirement.** | `overflow_latches_gap_without_blocking_ack` — a saturated count/byte budget still acknowledges events; gap remains observable until a complete reconciliation clears it. |
| **C14. Duplicate configured listeners are refused before serving work.** | `duplicate_workspace_or_app_has_one_listener` — duplicate entries/tokens, duplicate reported app IDs and a second service using the same workspace lock cannot produce two serving listeners. |
| **C15. Normalization preserves message identity and origin, while edits and deletes create no new work.** | `normalize_members_bots_edits_deletes_and_file_stubs` — all supported event shapes produce bounded records or explicit omission/gap results; a forged member prefix cannot become an own-app post. |
| **C16. Name lookup failures remain visible and the cache remains bounded.** | `names_fall_back_to_ids_without_unbounded_cache` — failed lookups display IDs, failures do not become permanent successes, and excess distinct authors cannot grow the cache indefinitely. |
| **M1. Canonical hub folders and workspace/channel pairs form a one-to-one mapping.** | `mapping_refuses_aliases_duplicates_and_inert_anchor` — aliases collide, duplicate pairs fail, exact-folder matching holds and the inert default anchor cannot participate. |
| **M2. Missing and invalid mappings explain configuration without enabling delivery.** | `resolve_explains_unmapped_invalid_and_disabled_delivery` — responses distinguish unmapped, malformed, non-hub and absent `mail_after`. |
| **M3. Every action rechecks the canonical row and expected channel before admission.** | `stale_row_or_mapping_cannot_post_or_fetch` — removed/replaced rows and stale mappings reach no fake Slack mutation or file operation. |
| **M4. Slack work never holds the connection loop.** | `held_chat_request_does_not_delay_ping_or_other_ops` — a stalled fake Slack response leaves ping and unrelated operations responsive. |
| **M5. Watching cannot lose the snapshot/event overlap silently.** | `watch_snapshot_overlap_and_lag_resynchronize` — an edit/delete during snapshot loading wins over stale snapshot content; bounded overlay overflow requests resync. |
| **M6. An old daemon’s unknown chat operation leaves other panes usable.** | `old_daemon_chat_refusal_preserves_other_requests` — the actual unknown-op response becomes an unsupported-chat state; ordinary requests still complete. |
| **M7. Remote chat uses the window’s existing host link.** | `remote_chat_adds_no_link_or_login` — the link fixture carries chat, thumbnail and preview traffic without another connection route. |
| **M8. Chat operations require the existing admitted hello.** | `protocol_operations_are_refused_before_hello` — real first-frame requests, including chat and unknown/malformed controls, receive refusal and closure. |
| **D1. Channel mail is one bounded, marked record and untrusted content never becomes terminal input.** | `hostile_chat_is_one_marked_mail_record` — controls, direction characters and forged headers remain quoted/escaped within the serialized cap; the input sink receives only the fixed wake. |
| **D2. Member and drawer-person posts become work; row and other bot posts remain display-only.** | `origin_filter_delivers_people_without_row_loops` — event and history forms exercise all four origins, including file-share comments. |
| **D3. Existing wake policy is retained, including the Codex limit.** | `chat_mail_wakes_free_claude_and_never_types_into_codex` — the real filer/poll/wake path wakes a free Claude fixture, leaves drafts/dialogs/working rows alone and files Codex mail without prompt typing. |
| **D4. Progress advances only after every required page and filing succeeds.** | `incomplete_recovery_or_filing_keeps_boundary` — held locks, absent joined rows, omissions, limits and interrupted traversals retain the previous durable boundary. |
| **D5. Recovery includes replies to retained parents older than the boundary.** | `recovery_finds_disconnected_untracked_old_thread_reply` — an old untracked parent receives a reply during disconnection; the complete scripted traversal files it before progress advances. |
| **D6. Row reply/post verbs use current mapping and never retry an unanswered mutation.** | `row_verbs_target_current_thread_and_submit_once` — the shell fixture records encoded requests, thread selection, refusal and unknown outcomes; no second submission appears. |
| **F1. Reservations bound all managed bytes, including partials and thumbnails.** | `media_reservations_bound_active_and_completed_bytes` — concurrent transfers and eviction cannot exceed B; held readers/transfers prevent eviction of their files. |
| **F2. Managed writes and removals cannot follow substituted directories or leaves.** | `managed_media_refuses_symlink_and_reparse_substitution` — native swap fixtures on each OS observe no writes or removal outside the held managed directory. |
| **F3. Managed storage is ignored by Git and excluded from watcher registration and events.** | `chat_folder_is_ignored_and_unwatched` — real Git status and watcher observations establish both properties; no source scan is used. |
| **F4. Evicted files refetch by file ID or fail explicitly.** | `evicted_attachment_refetches_or_reports_unavailable` — a removed cache entry is materialized again, or returns a visible Slack failure. |
| **F5. Remote image opens and large-file opens use existing preview/docs paths.** | `remote_attachment_uses_preview_and_ordinary_docs` — the window has no local attachment path; full bytes reach preview, and an over-cap file uses a scope-`none` docs grant. |
| **F6. `show-result --post` reports showing and publishing separately.** | `show_result_posts_requested_bytes_despite_show_failure` — a missing window does not suppress the requested upload; either failed/unknown requested operation yields failure. |
| **U1. Paging and new arrivals preserve the reader’s anchor until jump-to-latest.** | `older_pages_and_arrivals_preserve_anchor` — inserts, resizing and arrivals retain the visible message; explicit latest follows the newest page. |
| **U2. One thread view owns its parent and compose destination.** | `thread_view_compose_and_back_keep_target` — paging replies and returning to channel cannot post into the wrong thread. |
| **U3. Chat text, records, drafts and thumbnail storage remain bounded.** | `chat_view_and_thumbnail_limits_hold` — workloads exceed every cap and observe bounded retained state and off-render image decoding. |
| **U4. Chat and preview share flowed-text selection semantics.** | `chat_and_preview_copy_same_utf8_ranges` — wrapped Unicode selections copy identical text without synthetic wrap newlines or media placeholders. |
| **U5. Slack text is shaped as text through existing bidi ingress.** | `chat_markup_and_hostile_text_remain_literal` — markup cannot execute HTML or fetch arbitrary images; each displayed line uses the existing shaping policy. |
| **U6. Enter submits once and clears only an acknowledged draft.** | `compose_retains_refused_limited_and_unknown_drafts` — repeat input and disconnects produce no automatic resend. |
| **U7. Late replies require the issuing host, canonical row, channel and current request.** | `late_chat_reply_cannot_install_into_another_view` — delayed pages, posts and thumbnails cannot change a replacement row or thread. |
| **U8. The real drawer presents the required workload without missed frames.** | `chat_200_messages_10_images_scroll_without_missed_frames` — the native pane-timing route enters the workload and records actual presentation; missing prerequisites fail explicitly. |

**Transport, records and bounds**

Use one HTTP client and one shared TLS policy per chat service. Disable automatic redirects, ambient proxy selection and mutation retries. Library-level retry behavior must be checked in the selected sources and challenged with the fake’s request counter; calling an API helper once is insufficient proof.

A single operation deadline covers connection, response headers, redirects, dependent metadata/name calls and streaming. Caller disappearance does not remove it. Keep the approved plan’s outer metadata/file bounds; delete child-only deadline layers. Reads may become eligible again according to Slack’s retry header. No durable outgoing queue or automatic mutation replay is added.

Initial resource bounds are:

| Resource | Bound |
|---|---|
| Admitted API operations | 16 per daemon, including at most two file transfers |
| Raw metadata response | 1 MiB |
| Websocket message | 1 MiB |
| Pending socket events | 256 entries and 8 MiB aggregate |
| Normalized record | 32 KiB encoded JSON |
| Message text / file summaries | 8 KiB / eight |
| IDs / timestamps | 64 ASCII bytes / 32 bytes |
| Author name / filename / MIME | 160 / 256 / 128 bytes |
| Cursor | 1 KiB, bound to its complete original query |
| Display page | Up to 20 messages; a thread parent may make **21 records** |
| Name cache | 1,024 entries |
| Transfer chunk | 64 KiB |
| Drawer channel/thread state | 256 records or 2 MiB text combined |
| Draft | 8 KiB per existing row |
| Thumbnail | 256 KiB encoded, one megapixel decoded |
| Thumbnail textures | 64 MiB |

The thread-page allowance corrects route A’s blanket 20-record claim. The shelved API permits a parent in addition to the reply limit.

The shared wire record keeps workspace/channel, exact decimal `ts` and `thread_ts`, author ID/name, origin, text, truncation, reply count and file summaries. Retain `latest_reply`, `limited`, `omitted` and `files_omitted` when applicable: they serve recovery and honest display. Timestamps are validated decimal strings, never floating-point values: exactly `<seconds>.<six digits>`, and a bound with any other number of fraction digits is refused before it is sent (live in the maintainer's workspace, 2026-11-01: `conversations.history` with a seven- or ten-digit `oldest` answered 0 messages, `has_more: false`, `ok: true` and no error, so a bad bound reads as an empty channel; the shelved Julia validator accepted 1 to 10 digits, a defect not to copy). Rule and test: `a_bound_without_exactly_six_fraction_digits_is_refused_before_sending`. Private URLs are absent.

Keep route A’s operation family: `chat.resolve`, `watch`, `unwatch`, `history`, `post`, `fetch`, `post_file`, with `changed` and `status` events. Requests carry the canonical row and expected channel pair. These are additive operations; existing payloads remain unchanged and protocol version **4** remains appropriate.

Watch registration precedes snapshot retrieval. While a snapshot is pending, the model retains a bounded overlay of live changes; it applies that overlay after the snapshot. Overflow forces resync. Server bus lag must send a chat resync status instead of inheriting the current generic “log and drop” behavior. No chat epoch or sequence protocol is needed.

**Fake Slack and the TLS seam**

The test suite runs a scripted Slack service on loopback. It implements only the HTTP behavior the client exercises and uses the selected websocket server handshake for Socket Mode. All listeners and tasks belong to the fixture and are joined or cancelled through their recorded owners.

Use fixed, explicitly test-only CA/server fixtures and `tokio-rustls`. The product constructor always uses the product trust roots and validated Slack destinations. A test-only constructor may inject the test CA and a resolver that maps the fixture’s logical hosts exclusively to loopback; every other resolution is refused. Websocket tests use the same logical name, certificate verification and shared TLS configuration.

The test constructor is available only under test configuration or an explicitly test-only feature. Installed binaries expose no endpoint override, insecure mode or test-root setting. There is no plaintext escape hatch in the product.

The fake scripts:

- Channel and thread pages, parent-inclusive pages, repeated/missing cursors, malformed JSON and limited history.
- HTTP rate limits with mixed-case retry headers.
- Private-download redirects, relative locations, forbidden destinations, oversized bodies and sign-in HTML.
- Upload allocation, incremental byte receipt and one completion call.
- A post accepted before its response connection disappears.
- Socket hello, numbered envelopes requiring acknowledgements, warning, refresh and disconnect.
- A peer that stops answering websocket close.
- Slow name/file metadata while new envelopes arrive.
- Event overflow and reconnect recovery.
- An old parent with a new reply outside the recent channel-page range.

Certificate rejection and hostname rejection are separate tests. The normal suite reaches no Slack service and requires no credentials or internet access.

**Delivery, recovery and cutover**

Chat uses `comm::mail::filer::file_comm`, which already exists as an in-process entry. It adds no inbox writer or channel comm identity. The daemon adapter resolves the current joined row and bound handle immediately before filing.

Mail uses an empty, non-routable sender and a daemon-generated origin header containing channel, thread, author and stable `(workspace, channel, ts)` identity. The complete serialized record is capped at 64 KiB. Attachment paths are named only after successful materialization; failures remain explicit.

The shipped instructions state that channel text is untrusted project input under the repository’s allow list and hard rules. It cannot approve pane actions, reopen a refused action or authorize token-file reads. Only messaging’s existing fixed wake reaches the terminal. Codex receives mail with the approved no-idle-wake limitation.

One durable boundary per channel lives under daemon state, initialized from `mail_after`. It advances after a **complete** reconciliation pass and successful filing, never from a received socket timestamp alone.

Recovery must enumerate retained parents older than that boundary. The live gate selects the simplest demonstrated traversal:

1. Discover retained channel parents, including old ones.
2. Use proven reply metadata, or traverse their replies where necessary, to establish a horizon from timestamps Slack actually returned.
3. File the required interval with one fixed upper horizon.
4. Commit only when every required page and filing succeeded.

Do not assume the newest channel-root timestamp bounds replies to old threads. Do not substitute local wall-clock time for a proven Slack horizon. Process discovery and filing with bounded memory; no durable parent catalog is added.

An omitted record, inaccessible thread, limited required history, absent row, failed filing or interrupted pass retains the old boundary and reports incomplete recovery. Replays may repeat an origin ID. The guarantee is no silent loss within the verified exposed history, not exactly-once agent effects or recovery of deleted/retention-expired material.

For each moving channel, H0/H1/H2:

1. Pass the new app’s gates and exercise viewing, posting and figures with agent delivery disabled.
2. Record a conservative overlap boundary.
3. Quiesce every old admission route and inspect actual accepted-work outcomes.
4. Persist retirement of primary, listen, scheduled and proactive routes.
5. Enable the new boundary and reconcile the overlap.
6. Account for numbered posts, attachment-only messages and old-thread replies across the handover.
7. Verify that restart/deployment cannot recreate the old route and other channels still work.

PR 18’s `complete` means no snapshotted task remains running and no scheduled firing remains in flight. Its implementation and documentation explicitly allow failed tasks or pending schedules alongside `complete`; task completion does not establish a successful Slack response. Handover therefore requires per-origin outcome evidence.

**Figures and the drawer**

Keep `.sot-chat/` under the mapped root. It is the one chat-managed attachment store, bounded by B, with generated names, a self-ignoring `.gitignore`, reservations, partial cleanup and eviction of completed unheld files. No second cache limit is added.

Platform supplies the narrow native operations needed to create, publish and remove fixed managed leaves without following replacement links. Existing Unix directory pins provide a foundation; platform’s current Windows contract explicitly lacks handle-relative descendant opens. A pathname check followed by a write does not establish the required guarantee. B4 must supply and prove the missing native mechanism, or stop that capability on the affected platform.

`chat.fetch` materializes an original or thumbnail, then obtains a Files node through the existing adapter. Existing binary paths remain:

- Thumbnail bytes through `file.download`.
- Full image through the result/preview path.
- Files beyond preview capability through ordinary `docs.open`, reply scope `none`, and the existing remote proxy.

The current preview binary cap is **512 MiB**, defined in `rust/protocol/src/ir.rs`. The large-file witness must exceed it while fitting B.

Uploads retain the shelved branch’s three stages. File shares use the person/row label in `initial_comment`; they do **not** promise the customized text-post author or metadata. A separate attribution post would add another uncertain mutation and is unnecessary.

`show-result --post` performs both requested operations, reports them separately and attempts publication even when showing fails. Ordinary `show-result` retains its current behavior, including the recently changed page-offer path.

The drawer keeps channel history and one thread view, newest at the bottom, explicit compose destination, anchor-preserving paging and jump-to-latest. It displays author/time, literal Slack text, bounded thumbnails, edit updates and delete tombstones. Shared selection preserves preview’s Unicode and soft-wrap semantics. Image decoding occurs outside rendering.

**Live gates and their consequences**

SlackClaw’s session runs these with its Julia harness on the dedicated app in the maintainer's workspace. Results reach this session through the captain. This planner neither runs them nor claims their results.

| Gate | Design dependency | Failure consequence |
|---|---|---|
| **Socket Mode from the hub** — `chat_socket_receives_as_daemon_account` | The daemon account can reach Slack and receive events for the app. | Stop that workspace’s live service; do not add a child or another host route. |
| **Private download** — `chat_private_image_download_matches_uploaded_bytes` | Required scopes, actual redirect destinations, authorization and rendition access. | Stop inbound figures until corrected. Any destination-policy amendment follows the observed redirect evidence. **Live in the maintainer's workspace 2026-11-01: single hop, `files.slack.com`, 200, no redirect; passes with the pixel rule below.** |
| **Upload round trip** — `chat_png_upload_round_trips_pixels` (was `..._exact_bytes`) | Allocation, byte upload, completion, thread placement and file-share attribution. | Stop outbound figures; do not report the figures done test complete. **Live in the maintainer's workspace 2026-11-01: upload and share work, but Slack strips a PNG's text chunks (a 217-byte PNG came back at 101 bytes, its `tEXt` chunk gone; `IHDR` and `IDAT` byte-identical).** The rule: an image round trip promises its pixels, not its bytes; the gate and the client's own check compare decoded pixel data (or the image chunks), and nothing in the window or a row relies on metadata inside an image surviving Slack. |
| **One listener per app** — `chat_dedicated_apps_receive_every_numbered_post` | Dedicated app ownership avoids event-stream splitting. | Correct ownership before enabling delivery. Local locks cannot detect an unrelated external listener. |
| **Old-thread recovery** — `chat_bot_pages_history_and_recovers_old_thread_replies` | Bot access to public/private thread replies, retained-parent discovery and the recovery horizon. | Stop recovery-dependent mail activation and cutover. No user-token substitute or silent recent-history limit. |
| **Names and file stubs** — `chat_apps_resolve_authors_and_private_file_stubs` | `users.info`, `files.info`, channel sharing evidence, attachment-only posts and small renditions. | IDs and explicit unresolved stubs remain displayable; affected name/figure capability is incomplete and cannot satisfy its live done witness. |
| **Customized author** — `chat_custom_author_keeps_app_badge_and_round_trips_origin` | Person/row distinction survives phone display, Socket Mode and refetched history. | Stop person posts until corrected; there is no other posting form. **Live in the maintainer's workspace 2026-11-01: 12 of 12 checks pass, and on the phone both custom-named posts keep the app badge.** |

The candidate scope set remains `channels:read`, `channels:history`, `groups:read`, `groups:history`, `chat:write`, `files:read`, `files:write`, `users:read` and `chat:write.customize`; events are `message.channels` and `message.groups`, with an app token holding `connections:write`. This is recorded by PR 21’s manifest, **not live-verified authorization**.

**Settled by the live tests (the maintainer's workspace, closed 2026-11-02; the facts note is SlackClaw's `dev/output/slack-api-facts.md`)**

- **Posting form.** A person's post goes out under a custom author name with the app badge. The text-prefix path is gone: no posting-form setting, no switch on a definite rejection, no repost in another form. File shares keep the person/row label in `initial_comment`, which is attribution on the share, not a second posting form.
- **One workspace.** The maintainer decided his one workspace is enough for now, with no app in a second. The client runs one listener and `chat.toml` holds one workspace; a second is refused at load, and C14 reads that way. Workspace and channel IDs stay in every record and wire message, because they are part of a message's identity, so a second workspace later adds a listener and no migration. There is no per-workspace service map until then. The counts above are not recounted for this.
- **History bounds.** A bound carries exactly six fraction digits. Slack silently returns an empty page for a longer one, so the client validates a bound before sending it.
- **Late replies.** A late reply in an old thread never appears in channel history. Recovery pages back past the boundary, picks the parents whose `latest_reply` is newer, and reads each picked thread with `oldest` at the boundary. Gate B passed this way in the maintainer's workspace.
- **Images.** A round trip promises pixels, not bytes, as the upload gate above says.
- **Rules no live test reached.** The client keeps the documented handling for a history page with `is_limited` set, a download that redirects to another host, and a shared-channel file stub.
- **Pause switch.** The SlackClaw session's merge ask comes when the first channel is about to move.

**Validation and scan inventory**

The write set was checked against `dev/output/briefs/lanes-066/scan-inventory.md`.

Adding protocol operations touches **R28**, whose current `rust/backend/tests/admission.rs::op_names` reads source text. Replace it in B2: the protocol’s single op declaration emits constants and a canonical operation fixture, and the existing real refusal/closure loop consumes that fixture. Include unknown and malformed first-frame controls.

B3 calls the existing filer without changing its append policy, so it does not extend R01. The old UI R11–R13 scan module is absent from the current tree; preserve current behavioral owner tests. Recheck the inventory against each final write set because the inventory names several pre-reorganization paths. No landing adds or broadens a source-text behavioral proof.

Each Ship of Tools landing requires the local gate, workspace build, touched suites and its new behavioral proofs before push. Register the new crate and suites in the existing hosted coverage scheme. TLS dependencies additionally require the three-system compiler-only build evidence and the Linux release-artifact smoke. B4 requires native filesystem substitution proofs; B5 requires actual native presentation evidence.

The final integrated witness covers:

- Drawer ↔ phone exchange in the maintainer's workspace, including a remote window viewing the same hub row.
- Figures both ways with byte equality, thumbnail/full preview and the over-cap docs path.
- Multiple history pages, bounded retention, jump-to-latest and an old thread.
- Window-closed mail, safe wake and an in-thread response.
- Retired fleet routes and per-origin handover accounting.
- The brief’s required landing records, actual line deltas and run evidence.

**Risks the original comparison did not name**

- **Bot-token thread access is a possible scope blocker.** The shelved branches’ fake replies do not establish access to the actual channel types.
- **Full old-thread discovery may dominate API traffic.** Avoiding a durable parent catalog keeps the design smaller, but rate limits or retention can make complete reconciliation impractical.
- **A bounded event count alone is insufficient.** Raw websocket size, total queued bytes, names and error bodies need independent bounds.
- **A snapshot can resurrect stale content.** Watch-before-fetch needs an overlap rule for edits/deletes, not merely timestamp deduplication.
- **TLS/root choices affect deployment.** Private interception roots and Linux musl packaging need actual proof.
- **Existing lock entries can grow the daemon.** URL parsing alone makes additional ICU-related packages reachable.
- **Windows managed storage needs a real native mechanism.** Current platform guarantees do not establish the proposed symlink/reparse safety.
- **File-source metadata checks are not an immutable snapshot.** They detect ordinary changes; the plan does not promise atomic capture against another writer deliberately manipulating the source.
- **In-process faults share the daemon’s failure domain.** Bounds and sanitized errors reduce exposure; route B has no process-isolation boundary around the client.
- **Dedicated apps do not prove exclusive global ownership.** The local lock covers cooperating daemon instances, while the live gate and provisioning procedure cover external listeners.

**Maintainer decisions before code starts**

1. **Client stack:** minimal reqwest + Tokio Tungstenite + Rustls/ring + bundled roots, subject to exact dependency and compiler-only verification; or request a Hyper-based design. **Recommendation: the proposed stack**, with unresolved facts closed before B1 lands.
2. **Posting form: settled.** The customized text author passed its live gate in the maintainer's workspace on 2026-11-01, so the prefix path is dropped; file shares keep their `initial_comment` label.
3. **Failed old-thread gate:** stop recovery-dependent activation/cutover; or explicitly revise the brief to accept a narrower history guarantee. **Recommendation: stop at the gate.**
4. **Attachment bound B:** 1 GiB per mapped folder; or another explicitly chosen bound large enough for the over-preview-cap witness. **Recommendation: 1 GiB as the initial single bound.**
5. **Codex participation:** mail without idle prompt typing; or defer Codex rows. **Recommendation: accept the stated mail-only wake limit.**