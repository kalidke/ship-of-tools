# The reorganization's passes, as measured

Moved here from `docs/package.md` on 2026-10-27 (the captain, on the maintainer's word): the package page holds the
current state and the standard, and no pass loads this history. Each row is what its pass took, in the words written
at its landing; the session-hour and dollar estimates the table once carried were removed the same day.

| # | pass | measured |
|---:|---|---|
| 0 | Mechanical preparation: tests out of product files into sibling files; the 117 folder pages (164k tokens) to one-line pointers, their full text kept in `docs/history/`; the 51 ADRs and their index (215k tokens) to `docs/history/adr/`, loaded by nobody; the root CLAUDE.md to this page; the size-limit and page checks retired; memory pruned to operating state; the four-number script | 0.7 h, about 30-35 dollars; product 1,959 -> 1,962k, tests 2,087 -> 2,034k |
| 1a | Distribution, the release path: `release.sh` without its test re-run or green-at-HEAD rule, one hosted run, the pack; done test: thirty minutes from fixes in hand to a candidate installed on the maintainer's workstation; the outside review | 1.6 h (pass 1.1, gate and landing 0.5); done test 22.4 min by segments; about $23 all-in; distribution 215/295 -> 213/298k |
| 1b | Distribution, install, update and launch (D5) | 10.4 h wall in six runs (the pass 2.9 h; five fix rounds after two outside reviews 7.2 h); about $157 for the pass's agent; distribution 214/298 -> 221/252k, twice 30 -> 56 (one function per system); `install.sh` 2,053 -> 216 lines, `install.ps1` 975 -> 215 |
| 3 | Rows, with work-state and how a row's state is shown | 4.2 h wall in two runs (the pass 3.7 h, its scan follow-up 0.5 h); about $78 for the pass's agent; rows 150/163 -> 162/182k (work-state moved in), messaging 117/285 -> 109/267k |
| 4 | Messaging (D1) | seven agent rounds: the pass 4.8 h (stopped at its boundary), then six short fixes of 1 to 45 min; about $93 for the agents; messaging 109/267 -> 94/124k, twice 20 -> 45 (the shell door's wrappers and generic names, two real pairs queued); comm shell 6,072 -> 2,571 lines, its shell tests 14,272 -> 3,850; about 160k tokens deleted, not D1's 300k (the PowerShell copies were already gone); the local gate 12 -> 6.6 min |
| CI | Distribution's CI: the hosted run at each landing about seven minutes and the local gate near five; the slow proofs to a release-gate run made once per release | eight short agent rounds, about 30 agent minutes in all; the rest was hosted runs (nine per-landing, five release-gate) and waits; about $13 for the agents; the per-landing run 48 -> 7.2 min wall measured alone (longest job 6.9 min), the release-gate run 21.5 min; the local gate 12 min, of which the bash 3.2 suites, which messaging deletes, are the wall; since 2026-10-22 the Julia package jobs install the preview tools (ghostscript, poppler-utils, ffmpeg) from a cache keyed by the runner image and the package list, after the Ubuntu mirror stalled five landing runs in one day: a hit installs in under 10 s and fetches nothing; a branch's cache is invisible to main, so main fills its own on its first run per runner image, which reads the mirror once, and on that miss a mirror that has not delivered within 140 s is replaced once by archive.ubuntu.com (proven by a forced run) |
| 5a | Platform and wire: their contracts; platform its own crate out of `sot-log`, its copies folded | 1.2 h wall in three agent rounds (the pass 25 min to a gate-ready head, the review round 6 min after an outside review, a landing fix 8 min), about 40 agent minutes; about $15 for the agent; platform 83/65/47/8 -> 79/80/48/0 (tests rose with the shared test helpers moved in; twice rose by one, generic names such as `context`), wire 42/14/3/2 -> 42/15/3/2; three hosted runs, the landing one 7.9 min |
| 5b-1 | Containment carved out of lifecycle into its own crate; lifecycle's contract (5b split by the load rule: 211k of code and tests) | 1.0 h wall in two agent rounds (the pass 23 min to a gate-ready head, the review round 5 min after an outside review), about 30 agent minutes; about $11 for the agent; lifecycle 42/67/39/15 -> 22/44/21/14, containment (new) 23/21/22/3; the landing run 12.2 min |
| 5b-2 | Topology: its contract; its own crate out of `rust/protocol`, which ends wire's edge to `sot-log` | 1.0 h wall in two agent rounds (the pass 21 min to a gate-ready head, the review round 5 min after an outside review), about 28 agent minutes; about $9 for the agent; seven defects fixed red first (unserialized `topology.set`, a sync that could overwrite a newer hub edit, three unchecked names reaching ssh or a socket path, a monitor label repeated across headers, a refused edit swallowing a hand edit); topology 50/52/35/26 -> 52/55/35/23, wire 42/15/3/2 -> 41/15/3/1; the landing run 6.9 min |
| 5c | Server: its contract; the op table into the binary, every service a handler with an explicit context | 0.9 h wall in two agent rounds (the pass 22 min to a gate-ready head, the review round 7 min after an outside review), about 29 agent minutes; about $7 for the agent; the op table was already one table and handlers already took explicit arguments, so the cut was the shared reply helpers and byte pump moved to wire, the server's submodules made private, pages and the updater given a closure instead of the roster; queue item 21 fixed on the real start path and the reaper fixed under event load, both red first; server 54/119/15/44 -> 52/118/14/45, wire 41/15/3/1 -> 43/17/4/1; the landing run 9.3 min |
| 6 | Capsule (D2), in passes from the plan: 6a voyage out, 6b one process per row, 6c-1 the row socket in four landings (a1 the socket served beside the old attach lane, a2 the window, wake, topology and bridge switched with the one protocol raise and the single dial, b the daemon's calls on the socket and the old lanes out, c rows on the socket in three: c1 rows read runs and the fence file, c2 the voyage folder out, c3 in two: c3a the watch served and the wake's debt per run, c3b the daemon on the watch, the adopted-row exit and run identity), 6c-2 in two landings (a the 0.7.0 retire of 0.6 rows and the launch's two dead flags, b the dormant drawer, rows' drawer arm, messaging's voyage handle form and the false lane prose), 6d the transport home, the capsule parser's dead flags and the contract | 6a: 1.4 h wall (07:43 to 09:10) in four agent rounds (the pass 34 min to a gate-ready head; one review round 8 min; two landing fixes, a test-only re-export the normal build could not see and two screen tests that read only the visible screen on Windows), about 45 agent minutes; about $35 for the agent; capsule 410/403/143/3 -> 341/345/135/3 (69k product and 58k tests deleted); the landing run 6.9 min; 6b stopped at its 45-minute line at 10:10 (49 min, 8 of the rows suite red: a plan gap at the rows boundary, not size), its capsule half kept on its branch; the amendment 7 min, $1.19; 6b-0 (the rows suite reads only the surface that survives 6b, no product code) 2.3 h wall (10:27 to 12:47) in two agent rounds (the pass 16 min to a gate-ready head; one review round 18 min after an outside review that found four tests that would still break on 6b and a pid-reuse race in the new kills, which now go through a pidfd), about 38 agent minutes; about $8 for the agent; rows 162/182/40/45 -> 162/181/40/45, capsule 341/345 -> 341/346; the wall was mostly hosted runs queued behind two fixes on main; the landing run 9.0 min; 6b (one process per row) landed 17:11: the first run stopped at its 45-minute line (50 min, about $14) on five design gaps found after the plan, an 18-minute read: the watchdog's restart budget (the deleted anti-flap counter had been what turned an unlaunchable agent terminal), the rows integration suite left out of the load (it read the deleted internals), the journal's barrier at end_run admission (an acknowledged end could resume as a new run), the lane-error arms (a fatal attach transport rested ended_no_respawn; a terminal supervisor left its agent running) and the macOS hangup limit; the amendment 7 min ($1.19), 6b-0 ($8), then the relaunch 3.5 h wall in four agent rounds (the pass, three rounds after the gate and an outside review with four blockers, all fixed red first) and three landing fixes (the budget tests' backoffs behind a test seam; a macOS test's flag and its private run dir), about $31 for the agent; capsule 341/346/135/3 -> 273/315/130/3 (68k product and 31k tests deleted), rows 162/181/40/45 -> 163/183/42/45; the day went to 6b; 6c-1 was then checked against the code by a planner (11 min, about $2), which found it at about 540k counted whole, not the plan's share of 160k, and split it in four landings; 6c-1a1 (the row socket beside the old attach lane, additive) landed 19:23: 1.9 h wall (17:30 to 19:23) in two agent rounds (the pass 43 min to a gate-ready head, inside its clocks; one review round 15 min after an outside review with two blockers, a request queue a client could fill without bound and an eight-connection bound that freed slots before their threads ended, and seven fixes, all red first) and one landing fix by hand (the Windows pipe test opened a busy instance without retrying, as the product's client does), about 58 agent minutes; about $13 for the agent; capsule 273/315/130/3 -> 287/324/141/7 (twice up 11 while the old lane and the socket both stand; cross up 4 with the new edge to wire), wire 43/17/4/1 -> 43/18/4/1; the landing runs 7.8 min (Rust) and 3.9 min (CI); main's run after it found one more test race (a joined thread the kernel had not yet reaped, counted right after shutdown), fixed by reading the count until the reap, within 1 s; 6c-1a2 (the switch: every viewer on the row socket, protocol 4, one login per remote attach) landed 22:02: 2.6 h wall (19:25 to 22:02) in two agent rounds (the pass 74 min, 29 past its clock, with three local gates of its own; one review round 32 min after an outside review with two blockers, a terminal reply still written on the run's loop and queued input that could reach a replacement run, all fixed red first; two decisions taken there with the captain: a slow viewer is resynced with a fresh screen, never cut, and a row socket that cannot be bound exits 75 into rows' budget) and one landing fix found by a diagnosis (19 min, about $2.50): under a flood every resync's screen was kept as an answer, 64 of them cut the window, and the first Ctrl-C never reached the agent, which the test's retyped Ctrl-C had hidden; about 106 agent minutes, an hour of it waiting in the foreground on test runs; about $52 for the agent; capsule 287/324/141/7 -> 251/288/145/7, topology 52/55/36/23 -> 51/51/34/23, rows 163/184 -> 162/183; the old attach lane's server is left for its own landing; 6c-1a3 (the old attach lane out) landed 23:11: 1.0 h wall (22:07 to 23:11) in two agent rounds (the pass 31 min to a gate-ready head, inside its clocks; one review round 7 min after an outside review with no blocker: a row listener that died left the agent unreachable, an end left queued input to time out, and a Windows fidelity test deleted without its replacement, all fixed red first), about 38 agent minutes; about $14 for the agent; 61 files, 11k lines deleted; capsule 251/288/145/7 -> 198/225/141/7; 6c-1b was then checked against the code by a planner (11 min) and split in two (one pass loaded about 221k), and it found a trap the old plan missed (rows' phase cell orders on the voyage, so feeding it the run would latch a resting row after a reset; the voyage stays in status until 6c-1c); 6c-1b1 (management on the row socket) landed 01:13: 1.6 h wall (23:27 to 01:13) in two agent rounds (the pass 56 min, 11 past its clock: the brief named one test that read the old leg number and there were thirteen, plus four direct end calls that now pass the run; one review round 12 min after an outside review with two blockers, a Windows stop a silent peer could hang and a listener lost while the row rests that stranded the supervisor holding its fence, and the brief's own decision that a lost reply reads status, never built; all fixed red first), about 68 agent minutes, 45 of them waiting on test runs; about $17 for the agent and $2.60 for the planner; capsule 198/225/141/7 -> 189/227/142/7; 6c-1b2 (the lane remnants out) landed 02:13: 1.0 h wall (01:14 to 02:13) in two agent rounds (the pass 21 min, inside its clocks; one review round 3 min after an outside review with no blocker, which asked for a cancellation test that cannot pass through the row's own timeout, the malformed-wire exchange tests ported to the row's hello, and records that named deleted files) and one gate re-run (the agent built test programs in its gate's target folder while the gate ran, relinking the daemon under a comm test: GATE-TARGET-SHARED again) and one landing fix by hand (a join-deadline test it had moved from Windows to every system needed a 100 ms sleep inside 150 ms and missed on macOS; it now lets the shared deadline pass and requires an immediate refusal), about 24 agent minutes; about $15 for the agent; capsule 189/227/142/7 -> 118/140/99/7, the night's largest cut; six tests of platform's isolated-test harness went with the socket suite and are queued to platform; 6c-1c was then checked against the code by a planner (5 min, about $1), which found it at about 250k and split it in three, and found this series' brief gaps: the reauth restart file that reads the voyage was missing from the plan, and 31 test files read the voyage, not ten; 6c-1c1 (rows read runs and the fence file) landed 03:51: 1.5 h wall (02:21 to 03:51) in three agent rounds (the pass 42 min to a gate-ready head, inside its clocks, with the first-start restart defect closed by the fence read alone; one review round 4 min after an outside review with two blockers, an absence check that created the start marker so a never-started row ended at a close would resume at the next boot, and a lost pointer that resurrected a requested end, both fixed red first; a 6-min follow-up after the architect found the second fix still resurrected the end at a second restart) and one landing fix by hand (two sign-in poll tests asserted exact counts a whole-second clock cannot promise and went red once each under the gate's load), about 53 agent minutes, 36 of them waiting on test runs; about $13 for the agent; rows 162/183/43/46 -> 162/183/43/44, capsule 118/140/99/7 -> 118/142/99/7; the branch's hosted runs 7.9 and 8.7 min; 6c-1c2 was checked against the code again by a planner (14 min, about $4), which found five traps in its old section, the record of why the check earns its ten minutes: deleting the voyage folder's mkdir would have broken every Windows start (the only thing that made the state folder there), the run's lock was the volume proof's caller, an answered reset could be undone by a kill before the next run, and two tests rode the old folder; 6c-1c2 (the voyage folder out) landed 05:33: 1.4 h wall (04:10 to 05:33) in two agent rounds (the pass 49 min to a gate-ready head, 4 past its clock; one review round 8 min after an outside review with three blockers, all fixed red first: an end admitted after the agent ended by itself was answered `ended` unrecorded, so a restart ran the agent again; a reset whose record write failed ran on into a new run; a reset published by a deletion Windows cannot flush, now by a durable write), about 58 agent minutes, 30 of them waiting on test runs; about $16 for the agent; capsule 118/142/99/7 -> 116/141/96/7, rows 162/183 -> 162/182; two hosted reds were the runners' (a Windows shard cancelled at its bound after a 4-min Julia unpack, a macOS name lookup failure), the second re-run green; 6c-1c3 was checked again by a planner (20 min, about $6), which found it at about 195k and split it in two, corrected two premises (a run's identity cleared at the watch's end would count every exit never-ready; the observer is not the cell's one writer) and closed eight traps (among them an exit frame the server's own shutdown dropped, and an announcement under the supervisor's lock that deadlocked); 6c-1c3a (the watch served, the wake's Enter per run) landed 09:07: 1.0 h wall (08:08 to 09:07) in two agent rounds (the pass 21 min to a gate-ready head; one review round 10 min after an outside review with three blockers, all fixed red first except one regression test the old code also passed: each change was announced by rereading the state, so a quick end lost its `ending` report, now recorded under the lock and told in order; a wake's Enter could land in a run that replaced the one it checked, now input names its run and the row refuses another; a watch admitted during the exit got no exit code) and one hosted red outside the pass (a platform test bounded a panic's settling at 5 s and a Windows runner's backtrace print took 9.7 s; its bound is now 20 s against a 60 s deadline), about 32 agent minutes, 15 of them waiting on test runs; about $8 for the agent; capsule 116/141/96/7 -> 120/147/100/7, messaging 95/125 -> 95/126; the branch's hosted run 8.4 min; the row then moved to a second account at the first's weekly cap, on the maintainer's direction; 6c-1c3b was checked again by a planner (16 min, about $5), which closed three traps the first half made (a restart that raced the exiting supervisor's fence, a Windows row read stopped while its supervisor ran, a Windows exit 69 read stopped); 6c-1c3b (the daemon on the watch) landed 11:58: 1.4 h wall (10:35 to 11:58) in two agent rounds (the pass 30 min to a gate-ready head; one review round 15 min after an outside review with two blockers sharing one cause, an exit not bound to the supervisor that made it, so an old exit 69 could latch a healthy replacement terminal and an exit already restarted could be restarted again, now every outcome carries the row's spawn generation and is acted on only at it), and three fixes outside the pass (a wake test counted both daemons' boot looks, which the 2 s ready made two; a containment test read a dead process's pid reused by a thread as an error; the wake shard ran past its 5-minute bound with every test passing and three tests moved to wake3); the red commits were not run, their tests red by reading and compilation; about 48 agent minutes, 20 of them waiting on test runs; about $15 for the agent; rows 162/182/43/44 -> 164/187/43/44, capsule 120/147 -> 120/148; the branch's hosted run 7.6 min; 6c-1 is complete; 6c-2 was checked against the code by a planner (32 min, about $14), which found it at about 350k and split it in two, and found five traps: a 0.6 stop ends only the supervisor, so a retire that only stopped it would start a second agent beside the old one, both holding the row's mail handle; 0.6.6 records a person's end in its journal, not in an `ended` file; the start sweep never looks inside a registered row's folder; the old supervisor journals the retire's own end; a Cleanup boot ends rows without resuming them; the captain took the cut line at once (the parser's dead flags to 6d, about 228k to 184k) and had boot retire every 0.6 row at once rather than four at a time, so twenty rows cost the slowest one; 6c-2a (the 0.6 rows retired) landed 07:59: 1.2 h wall (06:49 to 07:59) in two agent rounds (the pass 35 min to a gate-ready head, inside its clocks; one review round 8 min after an outside review with three blockers: a retry after a daemon's death skipped the row's Linux scopes, a person's end seen only on the live old supervisor was lost if the daemon died after its stop, and a macOS kill by process number, platform's stated residual, now named in rows' contract; and five more: malformed journal entries read as no end, the retire's own end read as the person's on a retry, the day-later sweep racing a retire, a start marker the end path could create, and tests at those boundaries), about 43 agent minutes; about $14 for the agent; rows 164/187/43/44 -> 174/198/57/45 (twice up 14 with the retire's frame and lane names, deleted with it after 0.7.0), capsule 120/148/100/7 -> 120/148/101/7; the branch's hosted runs 7.75 and 7.5 min; 6c-2b (the voyage era's last arms out: the window's dormant attach-only drawer, rows' drawer arm and messaging's voyage handle form) landed 08:31: 0.5 h wall (08:01 to 08:31) in two agent rounds (the pass 9 min to a gate-ready head, both red tests red by behavior; one round of a minute after an outside review with no blocker, whose findings were a contract sentence that promised more than the comm commands do, since a 0.6 session's send and poll fail with exit 1 and its identity lookup may still answer its own old name, and leftover drawer comments), about 11 agent minutes; about $3 for the agent; 361 lines deleted and 78 added; rows 174/198/57/45 -> 173/199/57/45, fe-ui 390/183/69/30 -> 388/183/69/29, messaging 95/126 -> 95/127; the branch's hosted runs 7.5 and 8.3 min; 6c-2 is complete; 6d was checked against the code by a planner (29 min, about $10), which found the crate rename alone writes 204 files, about 860k by the load rule, not the plan's 110k, and split 6d in three landings (the homes, the name, the contract); the captain took its decisions: a scripted rename counts by its reviewed diff, a pass loads only the contracts it touches (the full base measured about 102k, not 60k), and one build-only release-workflow run proves the rename before a tag; 6d-1 (the homes: the local connection client, the shared test support and the log masking to platform; the capsule's two dead flags out) landed 10:08: 1.1 h wall (09:03 to 10:08) in two agent rounds (the pass 33 min to a gate-ready head; one round of 3 min after an outside review whose blocker was the source-text half of the new dependency test, against the rule that no test proves code by scanning its text: it went, and the dependency graph read through cargo stays as a guard; three of platform's new promises were narrowed to what holds on Windows and macOS) and one landing fix by hand (the hosted run's catch-all test group excludes platform's package and could not build the three tests that moved there; they are named in platform's group, queued SHARDS-REST-EXCLUDE), about 36 agent minutes; about $13 for the agent; platform 80/79/52/0 -> 100/89/54/0, capsule 120/148/101/7 -> 98/138/96/9; the branch's hosted run 9.4 min; 6d-2 (the name: the crate `sot-log` is `sot-capsule` in `rust/capsule/`, the fence module out of the old journal, the build stamp renamed, and the throttled relay's half-close fixed, queue item LANE-BRIDGE-RELAY-HALF-CLOSE) landed 11:32: 1.4 h wall (10:10 to 11:32) in two agent rounds (the pass 35 min to a gate-ready head, the rename a scripted map whose diff was reviewed, 230 files, the lock file changing names only; one round of about 10 min after an outside review that found no stale name in the workflows, shards, release and pack scripts, gates or installer, and asked for two guards that proved less than their names and records the map missed), about 45 agent minutes; about $11 for the agent; the release workflow ran once on the branch, build-only (its draft job skipped), with all 22 jobs green, the only proof of the renamed release build before a tag; capsule 98/139/96/9 -> 98/141/96/9, everything else unchanged; the branch's hosted runs 8.0 and 8.9 min; 6d-3 (the capsule's contract, 13 guarantees each with its test) landed 15:09: 0.9 h wall (14:13 to 15:09) in two agent rounds (the pass 14 min to a gate-ready head, with four new guards each shown red against its code; one round, the first run as a Codex job on the maintainer's word that agent work moves to Codex, 8 min, after an outside review whose findings were promises stated beyond the code: degraded starts, a Windows writer with no five-second cut, the alternate screen, timing totals that were component budgets, a derived ready an end can pass; the Codex job's sandbox could not reach the build host or finish its last commit, so the architect committed and gated), a hosted Windows red that was the new guard discarding saved history, fixed in the test; about $7 for the Claude agent; capsule 98/141/96/9 -> 98/144/96/9; the branch's hosted runs 7.5 and 7.4 min; the capsule series (D2) is complete |
| 7 | Frontend: State apart first, one short pass per cluster (the row record, focus and prompt, preview requests, the REPL and agent pane), each moving its fields into a type with private fields; then the five crate cuts (D4) as moves: app and render, chrome and input, navigation and session, agent pane and drawer, preview and control | planned 2026-10-25 by a Codex planner (18 min) against the code: State has 236 fields extended in 75 files, so 39 landings (31 owner passes, two preparations for an fe-net crate and the lease client, six crate moves, fe-net among them), 31 to 52 session hours, each pass near 180k; the plan is `dev/output/reorg/passes/7-frontend-plan.md` (local); 7a (`RowDirectory`: the row listings and canonical-id translation, private) landed 03:45: about 1.1 h wall (02:44 to 03:45), every agent on Codex (the implementer 16 min, a test compile fix, one review round 8 min after an outside review that said LAND with three notes: a kept-default control weaker than the charter, translation over every listed host named and pinned, and a helper queued to the result-ledger pass); the five owner tests each shown red by a deliberate wrong owner and green restored; fe-ui 388/183/69/29 -> 389/185/70/29; the branch's hosted run 8 min; 7b (`RowPresentation`: each row's label, root, work state, previous state, flash start, strip order and default slug, private, returning effects; the REPL's lifecycle and the declared-session memory left for their own passes, which would have taken it to about 195k) landed 05:43: about 1.7 h wall (04:00 to 05:43), a Codex planner check (15 min) wrote its brief at 184.8k, then the implementer 17 min, one review round 9 min after an outside review that found two tests weakened in the move (rank before time across tiers, cycling direction) and a flash start sampled once before projection, all restored; the 22 owner tests each shown red by a deliberate wrong owner Codex wrote and green restored; fe-ui 389/185/70/29 -> 387/187/76/29 (twice up 6: three `State` wrappers that keep the owner's method names until the application pass, and generic names such as `refresh`); the branch's hosted runs 9 min (Rust) and 12.5 min (CI); 7c (`ResultLedger`: the pending results, their result and attempt serials and the reveal, private, returning effects; the helper that cloned every pending id deleted, queued by 7a's review) landed 06:11: about 1.1 h wall (05:08 to 06:11), a Codex planner check (14 min, 184.8k), the implementer 18 min, which counted its own load at about 200.6k, over the 185k line, from broad searches that opened files beyond the manifest (the next briefs name the whole read set); one review round 4 min after an outside review that found two transition checks masked by the next step and a tuple to delete; the eleven ledger tests each shown red by a deliberate wrong owner and green restored; fe-ui 387/187/76/29 -> 388/192/78/29; the branch's hosted runs 11 min (Rust) and 19 min (CI); 7d (`AgentPane`: the agent pane's target row and attachment, terminal size, selection and drag, attach episode with its clocks and warnings, warm clients, held screen, feed, dial errors and discarded input, private, returning effects) landed 07:28: about 1.5 h wall (05:57 to 07:28), a Codex planner check (16 min, 183.2k), the implementer 35 min (about 180k by its count), a test expectation fixed (the held screen keeps its text; only the clipboard trims), one review round 12 min after an outside review that found an eviction test weakened, wheel presence left only to a release-gate fixture (now also a unit test through a narrow context port, so it runs each landing) and the attach log moved after the client stamp (restored), and a focus scan's false positive on the new test fixed in the test; the 20 owner tests each shown red by a deliberate wrong owner and green restored; fe-ui 388/192/78/29 -> 386/198/91/29 (twice up 13: the pane owner's methods keep the names of the `State` wrappers that call them, until the application pass); the branch's hosted runs 11 min (Rust) and 3 min (CI). Native evidence: the result fixtures and pane timing run only in release-gate.yml, never in a landing run, so 7a to 7d landed with it owed; it is in release-gate run 40197701006 on main at 711d83d4f (native minimized window with the result fixtures, pane timing and window close, green on Windows and macOS); from then on one run after every third pass 7 landing and before any landing on unproven pane, result or rendering owners (the captain, 2026-10-25), each landing naming its run; 7e (`ReplModel`: the REPL's rows, eval routing, log, input, run-file status, interrupt, scroll, pkg mode, history and the lifecycle 7b left, private, returning effects; its presentation left for 7f) landed 08:02: about 1.4 h wall (06:41 to 08:02), a Codex planner check (16 min, 184.2k), the implementer 18 min (184.1k by its count), one review round 6 min after an outside review that found a delayed demo eval resetting the scroll, run announcements tested around their frame, and diagnostics lost in the move, all restored; the 20 owner tests each shown red by a deliberate wrong owner and green restored; fe-ui 386/198/91/29 -> 384/205/94/29; the branch's hosted runs 9 min (Rust) and 4 min (CI); native evidence in release-gate run 40204152703 (on 7f's branch, carrying 7e); 7f (`ReplPresentation`: the REPL's images, image slots, scrollback pixels, visible window and build anchor, private, returning effects; one named defect fixed, P7f-image-alias: two rows with equal eval and frame ids no longer paint each other's image) landed 09:07: about 1.6 h wall (07:28 to 09:07), a Codex planner check (13 min, 180.45k, found the plan's 120-150k reservation too small), the implementer 30 min, a fixture fix (a wire field misspelled in a test), one review round 6 min after an outside review that asked for its own native evidence and for two image-cache rules a test could tell apart; the 30 tests each shown red by a deliberate wrong owner and green restored; fe-ui 384/205/94/29 -> 384/211/100/29; the branch's hosted runs 9 min (Rust) and 10 min (CI); native evidence in release-gate run 40204152703 (minimized window, pane timing and window close green on Windows and macOS); 7g (`FocusPrompts`: focus, the nav prompt, scale entry's prior focus, relaunch and the quit hint, private; the two focus source scans replaced by behavior tests and their file deleted; focus changes still leave the quit dialog open) landed 11:10: about 2.6 h wall (08:33 to 11:10; it waited behind the publish guard's fix), a Codex planner check (13 min, 180.4k), the implementer 24 min (185.0k by its count), one review round 4 min after an outside review that found two log events moved to a new target, the hint's clock sampled before its text, a test bypassing the owner's admission and a lost backslash case, all restored, and three dead pieces deleted; the 21 tests each shown red by a deliberate wrong owner and green restored; fe-ui 384/211/100/29 -> 384/215/105/29; the branch's hosted runs 17 min (Rust) and 7 min (CI); native evidence in release-gate run 40219701337 on main at 9cba599d1 (minimized window, pane timing and window close green on Windows and macOS); 7h (`LayoutHelp`: the drawer's tenant, maximize, wide preview, Help with its origin and peek, and the two pending Help starts, private, returning effects) landed 15:12: about 3.4 h wall (11:48 to 15:12), a Codex planner check (17 min, 183.9k), the implementer 56 min, three fix rounds of a few minutes (a moved log line that tracing 0.1.44 cannot parse with a dotted field after a target, now in its braced form; two tests that probed the peek's fade a nanosecond before its end, where the f32 opacity is already zero, as before the move), one review round 5 min after an outside review that found a vacuous selection reset, pane widths left unasserted and three always-empty effects (deleted), and asked for the pass's own native evidence; the 22 tests each shown red by a deliberate wrong owner and green restored; fe-ui 384/215/105/29 -> 386/223/121/29 (twice up 16: four effect ports counted at their traits and at `State`'s impls, owner methods that keep the `State` wrappers' names, generic names such as `query`); the branch's hosted runs 10 min (Rust) and 3 min (CI); native evidence in release-gate run 40227928042 on the branch at cf72d3472 (minimized window, pane timing and window close green on Windows and macOS); replanned 2026-10-26 as whole-window work (`dev/output/reorg/passes/7-whole-window-plan.md`, local), on the maintainer's word that no code change takes an hour: the per-cluster passes re-paid 100 to 200k of understanding three times each (planner, implementer, reviewer) and carried red-first tests and mutation sets for behavior-preserving moves; now one Opus agent with a 1M context loads the window's UI whole once (about 600k) and is continued across landings, a move is proved by the build and the existing suite, the native window run comes once per batch, and the crate moves are deferred; each landing records its change and its launch to main, over 30 minutes a failure with its cause; landing 1 (`PreviewRequests` whole, 7i-A's stage reused) change 20 minutes with the agent's first load, launch to main 38 (13:04 to 13:42; missed: the gates 8 and the hosted Rust run 12 after the head); landings 2 to 5 as one batch (`InstalledPreviewSource`, `ParkedViews`, `SelectedRow` with the picker's fields private), changes 12, 6, 7 and 6, main at 14:07, 35 after the batch's head (missed: the hosted Rust run 15, a publish-guard job that builds bash 3.2 from a live GNU download, red once and green on rerun, and the push of main through the guard 5): the twelve-owner milestone, native run 40392914124 on main; landings 6 to 11 as one batch (`NavCursor`, `TreeView`, `Navigation`, `RevealWalk`, `FileOps`, `DriftCheck`), changes 5, 6, 6, 5, 5 and 6, main at 14:26, the local gate passed at 14:15, the first batch landed under the maintainer's rule that hosted runs test main after the merge; the push of main takes about 3 minutes because the guard scans each new snapshot whole (a merge commit is one: 2,678 files, 215 s measured, three or four process starts per path and blob), which the guard's speed landing cuts to a fixed number of processes per push; landings 12 to 16 as one batch (`Editor`, `ConceptSlot`, `ShapedPreview`, `RasterView`, `PageView` with `FigureOverlays`), changes 14, 8, 15, 14 and 10, head 14:19, main 14:42 (the gate 12 and the push 3 after the head); landings 17 to 22 as one batch (`FigureStore`, `MarkdownMedia`, `ControlChannel`, `TerminalDrawer`, `StripMotion`, `NavSpill`), changes 7, 12, 5, 8, 6 and 6, head 14:36, main about 14:58 (the gate 15, waiting for the gate to free, and the push); the batches' native run is the milestone's, 40392914124 on main a45168983, green on Windows and macOS; held by the agent for later: the Monitor tenant (its native fixture) and the host view and page proxy (waiting on the one-login link's interface); landings 23 to 27 as one batch (`StatusLine`, `BatteryGauge`, `Pointer`, then two fixes of landings already on main, fixed forward: five logs moved in 12 and 15 had changed their target and keep it again, and eight owners that shared a module with `impl State`, so that their privacy was convention, live in their own modules, which exposed two reads past an owner), changes 14, 5, 5, 4 and 15, head 14:57, main 15:14; landings 28 to 30 as one batch (`ProxyArms`, `ActiveLink` with `HelloRefusals`, the installed source's markdown origin stated as kept, from the only review finding), changes 10, 8 and 2, head 15:06, main 15:30; `State` has 104 fields left of 236; the Monitor tenant then went ahead with its native fixture moved to the owner's operations, the host view (`conns`, `hosts`) waits for the link, and render resources take one lifetime owner whose drawing stages are its operations; from here each merge to main is recorded as one entry, the sum of its changes and its push to main (the captain, 2026-10-26); the merge of landings 31 to 37 (`MonitorPane` with its native fixture through the owner, render resources as three stage owners `Gpu`, `Canvas` and `Brushes` that drop in the original order, `StartupScript`, `CaptureRun`, `FramePacer`): changes 74 minutes in all, the gate 12 and the push about 3; `State` has 64 fields left of 236 (the host view's `conns` and `hosts` wait for the link; the application's own fields, the exit flow and configuration remain) |
| 8 | Sidecars (D3): 8a every child but the REPL, 8b the REPL and the pages channel | 8a: 2.6 h wall (08:49 to 11:25) in three agent rounds (the pass 28 min to a gate-ready head; one review round 20 min after an outside review that found two real defects, a workspace root interpolated into the kernel's Julia source and a line break splitting a Pluto command, both fixed red first, with the contract narrowed to the code; a 2-min tidy-up) and one landing fix (a test stand-in that read macOS's padded `wc` count), about 46 agent minutes; about $10 for the agent; sidecars 106/116/45/37 -> 106/120/45/37; twelve mechanism items queued; most of the wall was waits: a false local red from two gates sharing one build folder (queued GATE-TARGET-SHARED), and a hosted red in a capsule test race, fixed on main first by its own pass (c7c3d578b, 8 min, $0.33); three hosted runs, the landing one 6.9 min; 8b: 1.5 h wall (14:10 to 15:41) in two agent rounds (the pass 36 min to a gated head, with the port channel built and the REPL's guarantees settled; one review round 13 min after an outside review that found a blocked stdin write hiding a child's death and its grant, fixed red first, with two small fixes red first and the rest narrowed), about 66 agent minutes; about $9 for the agent; sidecars 106/120/45/37 -> 108/123/46/36, pages 36/48/11/27 -> 37/49/12/27; seven mechanism items queued; gates on the second gate host; two hosted rounds |
| 9 | Pages and files | |
| 10 | Agents: 10a the contract, the launch recipe, and the skills and work-state hooks only where a row is; 10b the launchers and the row CLIs | 10a: 2.3 h wall (15:48 to 18:06) in three agent rounds (the pass 20 min to a gate-ready head; one review round 13 min after an outside review that found four blockers, all fixed red first: a person's own skills removed by name at the first install, a linked skills folder followed, hooks removed by matching text, and sessions outside a row losing the mail check and the subagent guard; a 5-min round for the maintainer's decision that the four onboarding skills stay global) and two fixes by hand (a test's row id minted twice within one clock second, queued ROW-ID-MINT; two layer tests that asserted Unix paths on Windows), about 38 agent minutes; about $18 for the agent; agents 137/76/11/10 -> 139/78/11/10, distribution 221/254/60/23 -> 224/257/60/23; the landing runs 7.3 min (Rust) and 6 min (CI, its Julia package job re-run once after a package mirror stalled twice); a Windows red in the work-state tests was main's (a test fixture let its lock go before restoring the environment), found by a diagnosis, fixed on main (a5c724779); 10b (the launchers and the row CLIs) landed 19:56: 1.2 h wall (18:47 to 19:56) in two agent rounds (the pass 23 min to a gate-ready head; one review round 8 min after an outside review whose blocker was a guarantee promising more than agents owns: the work-state hooks and the Windows launcher's stop helper still read daemon files, now named as exceptions and queued with their owners; the rest narrowed or fixed red first; its first gate went red on one fixed sleep in a new test), about 31 agent minutes; about $12 for the agent; three tools that opened daemon files now ask the daemon; agents 139/78/11/10 -> 140/86/11/10; the landing runs 8.3 min (Rust) and 5.7 min (CI) |
| PG | Distribution: the private-name publish guard moves into the repository and the side repository retires; A the guard and clone enrollment, B the installer | A landed 23:32: 3.0 h wall (20:30 to 23:32), every agent on Codex (the maintainer's order): the planner check 24 min, which found 236k and split it, and found that the installer already ran `git worktree prune`, removed as its own commit; the implementer 30 min, then two test fixes, one review round (four blockers: a library that could not load let a push pass, inherited git selectors could enroll another clone, a UTF-16 list matched nothing, a vanished list could print its path) and four Windows rounds, all test assumptions of POSIX on Git Bash (path forms, permissions) fixed as one class or skipped there with the limit stated; a check against the real list by counts only caught that an ASCII-only rule would have refused it (its comments hold UTF-8), so the rule is UTF-8 without a byte-order mark; the CI job runs one suite per job, since Windows Git Bash took past ten minutes for all three; distribution 224/257 -> 227/275; B landed 07:07: 6.5 h wall (00:37 to 07:07), every agent on Codex: the implementer, three Windows rounds (a shared fallback pipe from a missing USERNAME, native paths, PowerShell's environment), a planner core review after the third, one review round after an outside review (the guard's children unbounded, the verdict silent on the guard), then landing fixes from the hosted runs: the first 30 s bound cut real Windows enrollments (now 60 s a Git read, 300 s the setup script), a diagnostic matrix showed the installer's process query takes about 26 s cold on a hosted Windows runner in any environment (speed, not the isolated profile), and the nine real-enrollment units and the install witness took minutes each there, so on the captain's word they run on Windows once per release, proven green before landing in release-gate run 40189400665 (units 31 min, witness 46.5 min); load 237k by the rule, 287k with the three hosted logs the Windows rounds read, over the 180k target and, with the logs, the 250k ceiling; queued: the installer-observation pass (a 0.7.0 tag blocker), one Windows shell-test harness, the setup script's Windows speed; distribution 227/275/58/23 -> 233/291/63/24 |
| 11 | Release gate: the full hosted matrix once, install on every box, the maintainer's use, the tag (0.7.0); the installers' remaining proof is here: an rc9.15 box's old prelude handing off to the first candidate on a backend-only host with glibc 2.31, and the maintainer's use on the maintainer's own boxes | |


# Moved from docs/package.md on 2026-11-06

The maintainer decided the package page holds only the current state (relayed 2026-11-06). What follows is the page's text as it stood, kept whole and unchanged: the draft contracts, the decisions, the integration-test assignment and the State section (the landing history). A landing's record goes here from now on, not to the package page.

## The draft contracts, as they stood

**platform.** The contract is settled (pass 5a): `rust/platform/CLAUDE.md`. Platform is the crate `sot-platform`,
which depends on nothing of ours, and every other crate depends on it. Guarantees, each with the limit the contract
states: a machine fact (state folder, config folder, host name) has one answer, and `host_name` errs, never guesses; a
publication is durable and never clobbers (the caller flushes its source); a lock is the kernel's; a held folder stays
while its maker lives; a descriptor is born close-on-exec (macOS under a birth lock); a local reply is trusted only
after the challenge; storage exhaustion is known by its native code; a volume is proved before a store opens on it; a
restart is paced; a directory pin opens only a directory, and private means private (both on Unix; on Windows each is
a stated gap); since 6d-1 a local connection is bounded, cancellable and reaches only this account (`transport::connect_own`
and the Unix socket and Windows pipe clients, moved from the capsule, with the Windows and macOS limits the contract
states), and a page secret or a Slack token is masked in the logs written through `RedactingWriter` (the daemon's and the window's).
It also holds the shared test support, under its `test-support` feature. Rests on: the OS. Owns no runtime state. Does not: start processes for anyone but the native launcher it
hands out.

**wire.** The contract is settled (pass 5a): `rust/protocol/CLAUDE.md`, the frames and payloads between our programs
(the lanes' frames are the capsule's, the sidecars' protocols the sidecars'). Guarantees: an envelope is at most 1 MiB
in every reader and writer; a blob is exactly its declared length, and the writer refuses a mismatch before a byte; a
payload grows only by defaulted fields, else the protocol integer rises and hello refuses the mismatch (the server's
check, tested there); a child's output is read by lines lossily, so no byte ends a reader; policy asks
`is_release_build`, never the version string; the shared grammars have one parser. Since pass 5c it also holds what the
services share for replies (`sot_protocol::reply`: the handler output, a bounded frame write, and `reject`, which is not
bounded) and the handoff byte pump (`sot_protocol::pipe`). Rests on: serde, the compiler and
platform. Owns: nothing at run time.

**topology.** The contract is settled (pass 5b-2): `rust/topology/CLAUDE.md`, the crate `sot-topology` (on wire,
platform and the capsule's lane client); the daemon's half stays in `sotd` until 5c's op table. Guarantees, each with
the limit the contract states: hosts.toml has one grammar and one search order, and its writers exclude one another
through one lock, so neither `topology.set` nor `sync` replaces a newer version; a name that reaches ssh from the Rust
tree is checked against `[a-z0-9][a-z0-9._-]*`, and no shell runs on this end; every ssh the Rust tree starts turns
sharing off (the shell bridge is a named exception); since 2026-10-26 the window reaches each other computer over one
login, its link, which carries its control connection, its pages and its rows as channels (two dial keys that name
one relay endpoint are two links, a stated limit), and one remote attach is one channel and no login; a stalled
channel never stops another (per-channel credit, opens taken in order); the link's end (a closed window, ssh's end,
the far input's end, or 90 s of silence) fails every channel and every dial in flight at once, ends every copy over
it whatever it is blocked on, and ends the far router with its input still open; a page's open and a lane dial are
bounded and typed; endpoints are derived by one function each; a unit sotd writes starts only where hosts.toml gives
it; every `systemctl` call has a wait budget; `sotd stdio-bridge` adds nothing to the stream unless its first frame
is `link.hello`, which makes it the far router of a window's link, each channel admitted by the daemon as its own
connection, so the router adds no path between accounts. Owns: the hub's
hosts.toml and the generated relay units. Does not: hold the window's or the daemon's connections (fe-net and the
daemon's link do).

**containment.** The contract is settled (pass 5b-1): `rust/containment/CLAUDE.md`, the crate `sot-containment`,
which depends on platform alone. Guarantees, each with the limit the contract states: a child started through `Signal`
is in its owner's kill domain (a process group on Unix, a job and a console group on Windows), and its tree is asked to
end when its owner lets go or the signal fires; the fire is permanent and a start racing it is never lost; a request is
checked and an observation fails closed; every wait is bounded on the OS clock, an output read included; on Linux the
guard subreaper drains the daemon's children within `DRAIN_BOUND` (the drain is bounded, not every death; what it
could not kill it logs). Starts are held by clippy's process-spawns group, with its named exceptions. Rests on:
platform (the descriptor sweep, the Windows job, the macOS birth lock) and the OS. Owns: the registry of contained
trees and the guard. Does not: decide when to end anything.

**lifecycle.** The contract is settled (pass 5b-1): `rust/backend/src/lifecycle/CLAUDE.md`, in the `sotd` binary.
Guarantees, each with the limit the contract states: a lease is granted only to a peer whose pid, creation time and
boot the OS reported at accept; `held.json` is the one record and a start's plan is a function of it and this boot
alone; only the last lease's departure decides, the latest intent winning until a `Close` commits; a close stops
accepting before it touches a row, ends rows only through rows' end API (a row not ended is counted), and ends within
`SHUTDOWN_BOUND` plus the terminal's fire and folder waits; every controlled end takes one terminal with fixed exit
codes (0, 1, 2, 75, 78, 101; INT and TERM by the signal). Rests on: containment, rows' end API, server's accept loop.
Owns: `held.json`, the leases, the terminal. Does not: end rows itself, contain a child.

**server.** The contract is settled (pass 5c): `rust/backend/src/server/CLAUDE.md`, in the `sotd` binary. Guarantees,
each with the limit the contract states: one daemon per state root while `run` runs; a start does not unlink a socket
a live daemon answers on (live means a reply within one deadline; a busy daemon is fenced by its state lock); a
connection of another account is dropped at accept before a byte is read; nothing is served before an accepted hello;
one writer per connection; a handler's failure is one frame for the ops that reach `finish_dispatch`; a peer that
cannot drain is dropped; a ping is answered whatever the role, and a silent pinger is reaped even under event load; an
op runs off the loop unless `INLINE` names it; an event reaches a connection as one frame, or is dropped when its bus
lags, and the connection is then told so by a `bus.lost` frame. Services reach the server only through their handler arguments: the compiler enforces the server's private
submodules, the rest is convention until the services are crates. Owns: the listener, the roster, the op table. Does
not: implement any op beyond hello, ping and the handoffs.

**rows.** The contract is settled (pass 3): `rust/backend/src/rows/CLAUDE.md`. Guarantees: a row's id survives every
daemon restart and every refresh of its label; a row's comm handle has one answer, `bound_handle`, by construction (the
declared handle is a cell private to `rows::handle`), and no two rows hold one; one guard per row; no second supervisor
over a live one (every start path reaches one launch, which takes the row's fence before it forks); no start begins
once the close has begun; a row's phase follows its current supervisor, ordered by its run (since 6c-1c1); since
6c-1c3b the daemon holds one watch per row, so an adopted supervisor whose agent exits is restarted within rows' one
budget with no selection, and every exit is bound to the spawn that made its supervisor; since 6c-2a a row a 0.6
release left is retired before it is started or ended (its old run ended before its old supervisor, its Linux scopes
ended, an end its person asked for kept, its old files moved aside and swept a day later), every such row at once at
boot; a folder
had a supervisor iff it holds the fence file, which nothing that only observes a row creates (Windows limit: the
supervisor takes it after its spawn); a row is reported ended only after a confirmed end or the destroy proof, and
lifecycle ends rows only through the end API (`rows::end`). Work-state is rows': one writer path (`sotd
work-state`, each write one step under the registry lock), two reader paths (`shown_of` for the window's
`workspace.list` and poll; `states` for pages, including sessions outside rows), and a row waiting on
its person reads red on every system. Rests on: capsule (supervisor, attach), agents (recipe), containment, platform
(fence). Owns: the registry, row tomls, state folders, work-state. Does not: run the agent's terminal (capsule), decide
when rows end (lifecycle asks), or read a screen for mail (messaging's wake). Its crate waits for agents, sidecars,
files, messaging and containment to be crates.

**capsule.** The contract is settled (pass 6d): `rust/capsule/CLAUDE.md`, the crate `sot-capsule`, which depends on
platform and wire alone. Guarantees, each with the limit and the test the contract names: one authority per state
folder (the fence); one process per row, and the agent dies with it (macOS: an agent that ignores SIGHUP can outlive a
killed supervisor); a row outlives its viewers and the daemon (unless a start degraded into the daemon's kill
domain); one socket per row, this account only, nothing before an admitted hello, the row number the one gate;
attach shows the screen at once, then every byte in order, and a viewer behind is resynced; one driver and eight
real connections; input written at most once into the run it names, or reported unknown; a restart shows the last
screen (not what only the alternate screen held); no disk write stops output; status, end, reset and stop are
bounded replies fenced by the run, and an end answered `ended` was recorded first; a watch tells recorded changes in
order and the exit code before a controlled exit; the exit code says how it ended (0, 69, 70, 75, 76); the
supervisor names its row in its argv. Rests on: platform (locks, durable publish, the local transport, process
birth), wire (frames), the OS pty or ConPTY. Does not: know about rows, mail or windows; rows owns the one restart
budget.

**messaging.** The contract is settled (pass 4, decision D1): `rust/backend/src/comm/CLAUDE.md`, written from decision
0031, which wins where they differ. Comm is one implementation, in `sotd`: the daemon runs it in process and every
session runs `sotd comm`, on any computer that shares a home with a daemon host; the commands sessions type
(`comm-send.sh`, `comm-poll.sh` and the rest) are thin wrappers that keep their words and exit codes. Guarantees: a
handle is derived, never chosen (the folder, the agent kind and the box; a row keeps its handle through the self file
the daemon pins), and has one inbox; a send ends in `filed` or `FAILED` and nothing else, `filed` only by the
appender after the line is on disk for a live handle, within 5 s or it fails, with no queue and no second route; only
poll moves a cursor; an idle row with mail is woken by its own daemon, which types one fixed line into a free prompt
and never into a dialog, a menu, a working session or a person's unsent text, and whose owed Enter belongs to the run
it was typed into (since 6c-1c3a); a busy session's Stop hook holds its
turn until it reads; users on one computer never see each other; a subagent, or an agent started inside a session,
has no mail identity; every registry write is one locked read-decide-write; comm reads its environment as a value it
is given, never the process's. The on-disk formats are 0.6.6's, so both versions share one comm folder. Rests on:
platform (locks), rows (the row's terminal and declared handle, for the wake), topology (the hub link). Owns:
`~/.sot-comm`, `sotd comm`, the wake. Does not: work-state (rows').

**agents.** The contract is settled (passes 10a and 10b): `agents/CLAUDE.md`, the launch recipe in
`rust/backend/src/agents/`, the agent layer the installer writes, and the shell tools (the launchers `ccb` and `ccx`,
the row CLIs, `sot-fe`, the GitHub sign-in). Guarantees, each with the limit the contract states: one recipe builds every Claude leg, and
every leg runs `--permission-mode auto`; a leg starts a fresh conversation unless a reauth or a person resumes one;
`claude` and `ccx` are found by absolute path, by one rule (Unix); a leg's environment is built in one place; an
account is a folder, discovered each time, and a named one never falls back to the default; folder trust is recorded
only under the declared prefix, and preparing it never stops a row; a Claude skill and a work-state hook exist only
where a leg of the recipe runs (the skills in a layer folder under the comm home, which the leg names with
`--add-dir`, the hooks in the leg's one `--settings`), except the four onboarding skills (`sot-install`, `sot-setup`,
`sot-statusline-setup`, `sot-gh-auth`) and the end-of-turn mail check and subagent guard, which stay global because
they serve before any row exists or every joined session; an install removes only exact copies of what an earlier
installer placed (a shipped version's hash, an exact hook command), never through a link; the shell tools reach the
daemon only through the wire, by an op over an endpoint or a `sotd` subcommand (the work-state hooks and the Windows
launcher's stop helper, which are not agents', still read daemon files and are queued with their owners); `ccb` carries
no recipe of its own and `ccx` builds Codex's argv; the tools run under bash 3.2 (Git Bash unproven); every call of
the sign-in has a total deadline; one list fixes where the tools install; no tool leaves a temporary file, within
messaging's signal limits; a skill body carries no `$` and a digit (a lint). That Claude Code loads an added folder's
skills is the maintainer's done test. Rests on: rows (the launch), messaging
(handle, the global hooks), distribution (the installer writes the layer). Owns: the launch recipe, the layer, the
launchers, the non-messaging skills, the row CLIs. Does not: run the agent's terminal, decide when a row starts or
ends, read or write mail, drive the window (fe-ui's control surface does).

**files.** Guarantees: node ids are root-relative with `/`; a mutation resolves inside the root or is refused; a write
whose version differs from disk is refused; delete is a trash; the watcher never watches the daemon's own trees nor
crosses a filesystem nor exceeds its budget. Rests on: containment (gio trash), sidecars (the kernel's previews).
Owns: `.concept/`, `.sot-trash/`, the watches. Does not: interpret file content.

**sidecars.** The contract is settled (passes 8a and 8b): `rust/backend/src/sidecars/CLAUDE.md`. The REPL announces
its browser ports through an injected capability whose receiver pages owns (`Allow` and `End` per owner and generation,
applied before the browser frame is forwarded, a stale generation refused), so sidecars names nothing in pages. To the
rest: per row a Julia kernel (previews, project scans) and a REPL (user code), per daemon Pluto
and MathJax, per monitored host a sampler; a caller waits at most a stated bound and never spawns or kills (Pluto's and
MathJax's deadlines live in the caller's wait, so a caller that is cancelled leaves none; queued); the kernel starts
without the user's startup files or default environment and is given its root as data, never as source; a child's
page server answers nothing without a secret; each Julia child has its own temporary folder, removed after its tree is
asked to end (the removal is attempted and logged; the death of descendants is not proven); on Linux a user-code child
has its own X display only it opens. Rests on: containment, platform (held folders), the julia resolver. Does not:
serve pages to the browser (pages).

**pages.** Guarantees: every listener is loopback and the bound port is the one named; nothing served without a grant
(token, nonce, secret then cookie); `.git` and `..` never served; a remote window's page rides its one link and ends
when the link closes. Rests on: topology (the window's link), sidecars (Pluto's grant), containment (quarto, git). Owns: the
grant tables (memory only). Does not: render documents itself.

**fe-net.** Guarantees: one task per dialled host; a remote host is one link, whose control session sends its hello on
channel 0 at once and again on each reconnect (the daemon's first-frame bound would end the link otherwise), and its
pages and rows are channels on it; each connect-path reply within 30 s or a disconnect naming the
step; a down ssh host costs at most two logins a minute; a pipe host is leased before its data connection; 90 s of
silence ends a connection. Rests on: wire, topology, platform's `Redial`, lifecycle's window lease. Owns: the
connection set and `session-<host>.json`. Does not: touch view state.

**fe-ui.** Guarantees: daemon events are applied only on the UI thread at the top of a frame, one arm per event kind
with no wildcard; other threads only wake the window; untrusted text reaches the shaper one bidi paragraph per line;
input for an unshown row is held visibly for Enter, and sent input names the run its screen showed; showing a result
never steals the view. Rests on:
fe-net, capsule's attach client, winit and wgpu. Owns: all view state and UI persistence. Decision D4.

**distribution.** Both contracts are settled, in `scripts/CLAUDE.md`. The release path (pass 1a): the cut tags only
on the maintainer's word, runs no tests and reads no hosted run; the local gate is `rc-gate.sh` plus
`user-manager-tests.sh`; CI only drafts. The installers (pass 1b, decision D5): `install.sh` and `install.ps1` are
bootstraps that fetch a pack, verify it against `SHA256SUMS` and run its `sotd install` (`rust/backend/src/install/`),
where every rule of install lives once for all three systems. An install ends in one verdict line, runs nothing it
has not verified, writes nothing before every gate has passed, never moves backwards unasked, writes `install.json`
last, whole and in the shape every earlier release reads, asks before it ends a live daemon, and pins the unit to its
hosts before any reload; a repair run moves nothing but the pin. An update arms only strictly above the installed
version; apply verifies before it mutates and restores on failure; launch fails open. Rests on: the release tag,
`sotd` subcommands (`update`, `same-version`, `topology`), GitHub releases. Owns: `$PREFIX`, the units, the comm
install. Proven only at the release gate: the Windows converge (an update with a window running) and an rc9.15 box's
old prelude handing off to the first 0.7.0 candidate on a backend-only host with glibc 2.31. Since the publish guard's
landing A, a push from an enrolled clone is refused when a new commit's snapshot (its files, paths and identities), a
pushed ref name or a tag holds an entry of the private list as a whole word, which lives per computer outside any repository
(`publish-list.txt` in platform's config folder); a refusal names the commit or ref and the list's line, never the entry;
the gate, release and pack entries enroll their clone first. Stated limits: a never-enrolled clone, `gh` text and
`--no-verify`. Since landing B, `sotd install` enrolls the installed base clone and the clone it runs from on every
system, records where the list lives without writing it, leaves a clone whose `core.hooksPath` points elsewhere named and
untouched, and names each checked clone in its verdict; every child the guard starts is bounded (a Git read 60 s, the
setup script 300 s). Windows proves the real-enrollment units and the install witness once per release, in the release
gate (minutes each under Git Bash on a hosted runner), and the three guard suites per landing. The installer's own process
listing and the Windows end-state `--version` asks are bounded at 60 s; a listing with no answer (cut, failed, empty,
or not in its program's shape, such as CIM's `null`, `{}` or `[null]` or a `ps` row that does not parse) is never "none
runs", and it stops a Unix run before it writes, leaves a Windows update armed, or fails the final check
(scripts/CLAUDE.md, Installers, "Bounded waits"). A push scans only commits the
destination does not hold: since 2026-10-25 the guard bounds its history scan by the push's old ids and by what the
destination advertises at push time (one `ls-remote` per push, read only when it would dial exactly the URL git hands the
hook; any error refuses), so a new branch or tag no longer rescans the history the server already has. Since 2026-10-26 a push costs a few
processes per round, not per object: the published history that bounds it is listed by one `git rev-list` with
replacements, grafts and the commit-graph off (a shallow clone, or a listing that fails, takes the raw walk), and the
new commits, trees and blobs are read in batches, one per generation and tree level, and matched in one pass per
stream, with the same bytes scanned and the same refusals; a push of main fell from about 3 minutes to 16 seconds
(measured on the maintainer's workstation: 96 s had gone to walking the history behind 462 advertised refs one commit
at a time, 82 s to the snapshot, one process start per path and blob). Since 2026-10-25 no entry stands as a whole word anywhere in main's tree (three
replaced in four files).

**records.** Guarantees: the manual says what is true now; a design not built is marked unbuilt; no private names.
After the reorganization the package page and the contracts are the only descriptive records a session loads; ADRs
move to a history folder.


## Decisions (settled 2026-10-20), as they stood

- **D1 comm is one implementation, in the daemon.** The shell client (88k product, 214k tests, the PowerShell copies
  and the parity tests) goes; the daemon is the one writer of inbox and registry and the one answer to "who am I";
  hooks and scripts call one `sotd comm` command (the binary reaches every box, servers included, through the shared
  home). Deletes about 300k tokens; the Rust half (124k) grows a little. Ships as 0.7.0.
- **D2 the capsule becomes a per-row supervisor.** The per-row supervisor process that survives the daemon today
  stays; the voyage record, the three lanes and the codec go. Scrollback is a bounded file, so a reboot still shows
  the old screen; recovery is resume by id. What is lost in use: scrolling a live row back past the bound (at least
  10,000 lines; a few hundred are used). Done test: in a row, `/resume` inside Claude Code lists and resumes that
  project's earlier conversations (the terminal carries its picker); a row survives a window close and a daemon
  restart; attach shows the screen at once. The resumes started from a command line (a reauth's `--resume <id>`,
  `ccb --continue`, `ccx --continue`) are argv to the agent, do not depend on the capsule, and are tested in the
  agents pass. Live rows end once at the 0.7.0 install. Existing voyage records are retired then, nothing converted,
  unless the capsule contract finds a reader: in the maintainer's use a row's terminal is not read after its session
  ends, and closing a row already removes its state folder with its voyage. Retired with them: the promise that a
  voyage is read forever, the codec seams and torn-tail recovery.
  Refined 2026-10-22 (the maintainer, on the capsule plan): attach keeps today's 200 rows of scrollback (the 10,000
  above was never in use: a fresh attach carried 200); the old screen after a restart is new behavior, built in 6a;
  old voyage folders are renamed at the 0.7.0 boot and removed by the start sweep a day later; the row socket speaks
  wire's frames, and the row hello carries its own row-protocol number, so a daemon update never strands a live row.
- **D3 sidecars.** The kernel, the REPL, Pluto and the monitor sampler stay; the Linux per-child X display stays (a
  security boundary). The dormant Claude SDK producer (`rust/log/src/claude.rs`, `claude-sdk-helper/`, 47k tokens,
  no product path runs it) was deleted in the capsule's pass 6a with the store it wrote to. MathJax's node child is decided in that pass on one fact:
  whether markdown with math is previewed in use.
- **D4 the frontend splits into five crates** along the existing event router (`drain_events`, one arm per event):
  app and render; chrome, input, help and persistence; navigation and session; agent pane and drawer (terminal, REPL,
  monitor); preview (image, markdown, editor, concept) with the control surface. Held to the cross-crate call count
  (the fourth number). Revised 2026-10-21, from an outside audit: no crate cut is possible while the window's `State`
  stands, one struct of 238 fields extended by `impl State` blocks in about 72 files, since Rust allows an inherent
  impl only in the crate that defines the type. So the frontend runs first as short passes, one per cluster of State
  fields, in the current crate: the row record (the window's half of how a row's state is shown), focus and prompt,
  preview requests, the REPL and agent pane. Each moves its cluster into a type that owns its operations and its
  invariants, with private fields and no open mutation (a row's identity, lifecycle and cleanup under one owner; a
  preview request's completion and cancellation under one), and replaces any scan over it. A cluster pass loads the
  State declaration, the files that touch the cluster, the invariants across clusters it takes part in and their
  tests; a cluster that does not fit with those is cut smaller first. Each is judged by whether the next change to the
  window needs less surrounding knowledge. The five crate cuts follow as moves.
- **D5 the installer's logic moves into Rust.** The rules written twice today (3.8k lines of sh, 5.6k of PowerShell,
  three byte-equal copies of two helpers) move into `sotd install`, which the pack already carries; `install.sh` and
  `install.ps1` shrink to fetch, verify and exec it. Distribution goes in two passes: 1a the release path first, with
  the thirty-minute done test and the outside review, then 1b the installer.
- The repo's committed permission settings (`.claude/settings.json`, step 0) allow its own build, check, test, lint,
  gate, pack, workflow-run and install commands and reading its releases; nothing in them tags or releases.

## Integration tests: which pass reduces each, as they stood

Each pass keeps one fast unit test per guarantee in its contract and its part of the one smoke per OS; an
integration scenario that sleeps on the clock goes with the fix it pinned. `rust/backend/tests` (26 thousand lines)
and `rust/capsule/tests` (the capsule's, `rust/log/tests` before 6d-2) are assigned by file:
- Distribution 1b: `shell_dial`, `unit_pin`, `same_version`, `window_start`.
- Rows: `capsule_workspaces` (5.7k lines, the row half; the capsule half goes with pass 6).
- Messaging: `comm_wake`, `comm_file`, `comm_send`, `hub_link`, `comm_cli`.
- Daemon core: `daemon_lifetime` (5.6k lines), `window_lease`, `contained_job`, `ancestors`, `admission`,
  `control_session`, `ping_reaper`, `live_socket`, `daemon_boot`, `daemon_stdout`, `subcommand_help`,
  `support_checks`, `active_frontend`, `stdio_bridge`, `topology_set`, `topology_sync`, `relay_refresh`,
  `status_integration`; and the platform ones: `connect_own`, `other_account` and `winhandle_windows` in
  `rust/platform/tests` since 6d-1, and from `rust/capsule/tests` `challenge_unix`, `challenge_macos` and
  `macos_kernel_facts`, which stay with the capsule because they serve the challenge from its row server or run its binary.
- Capsule: `lane_bridge`, `switch_latency`, `keystroke_latency`; from `rust/capsule/tests` `socket_unix` (5.8k),
  `pipe_win`, `wire`, `capsule`, `supervisor`, `fault_storage`, `fault_kill`, `fault_terminate`, `reconcile_matrix`,
  `golden`, `conpty`, `fe_client` (`attach_worker`, `e2e_socket` and `e2e_pipe` went with the attach lane in 6c-1a2
  and 6c-1a3; the row socket's suites are `row`, `row_limits`, `row_pipe`, `row_supervisor`, `row_flood`,
  `row_switch` and `fe_attach`). Under D2 (b) most go with
  the code they test.
- Sidecars: `repl_windows`, `child_folders`. Pages and files: `site_root`, `site_windows`, `preview_order`,
  `old_watcher`. Agents: `trust_declare`, `trust_premises`, `agent_exec`.
- Deleted with the dormant Claude SDK producer (D3): `claude_e2e`, `claude_rig`.
- The shared support folders (`support/` in both) shrink as their users go, in the last pass that uses them.

## State, as it stood (the landing history)

- The reorganization started 2026-10-20 from main at a25f3c39f, with v0.6.6-rc9.15 published and in use; the
  `release/0.6.6` branch takes fixes found in use, nothing else.
- D1 to D5 are settled (above).
- Step 0 landed 2026-10-20. Pass 1a landed 2026-10-21: the release path's contract is the first settled. Pass 1b
  landed 2026-10-21: the installers' contract is settled, and `sotd install` is the installers' body on every system.
  Pass 3 landed 2026-10-21: rows' contract is settled, with work-state. The CI pass landed 2026-10-21: a landing's
  hosted run takes about seven minutes, and the slow proofs run once per release (`release-gate.yml`, by hand). Pass 4
  landed 2026-10-21: messaging's contract is settled and comm is one implementation, in `sotd` (D1); live sessions keep
  the installed scripts until the 0.7.0 install. Pass 5a landed 2026-10-21: platform's and wire's contracts are
  settled, and platform is its own crate, `sot-platform`, depending on nothing of ours. Pass 5b-1 landed 2026-10-21:
  containment's and lifecycle's contracts are settled, and containment is its own crate, `sot-containment`. Pass 5b-2
  landed 2026-10-21: topology's contract is settled, topology is its own crate, `sot-topology`, and wire depends on
  platform alone. Pass 5c landed 2026-10-21: the server's contract is settled, and no service names the server's
  internals. Pass 6a landed 2026-10-22: the capsule's voyage record is gone and a leg keeps a bounded screen file.
  Pass 8a and 8b landed 2026-10-22: the sidecars' contract is settled, the REPL included, and the REPL reaches pages
  only through the port channel. Pass 6b landed 2026-10-22: one process per row, under the capsule plan's amendment 10
  (one restart budget, owned by rows; the supervisor's exit code says what happened), after pass 6b-0 made the rows
  integration suite read only the surface that survives. Passes 10a and 10b landed 2026-10-22: agents' contract is settled
  whole, and from the next install a row's leg alone loads the Claude skills and the
  work-state hooks, except the four onboarding skills. Pass 6c-1 runs in four landings, decided 2026-10-22 after its
  plan was checked against the code; its last (6c-1c3) closes the adopted-row limit rows' contract states. Pass 6c-1a1 landed
  2026-10-22: each row's supervisor serves the row socket beside the old lanes. Pass 6c-1a2 landed 2026-10-22: every
  shipped viewer attaches through the row socket, the protocol integer is 4, and from this landing no candidate is cut
  from main until 6c-2 retires the rows a 6b build left (the captain, 2026-10-22). Pass 6c-1a3 landed 2026-10-22: the
  old attach lane is gone. Pass 6c-1b1 landed 2026-10-23: the daemon manages a row over the row socket, and the
  supervisor lane is gone. Pass 6c-1b2 landed 2026-10-23: the old lanes are gone whole. 6c-1c runs in three landings,
decided 2026-10-23 after its plan was checked against the code, and 6c-1 is complete when the third lands. Pass 6c-1c1
landed 2026-10-23: rows read runs and the fence file, and no row reads a voyage or the pointer. Pass 6c-1c2 landed
2026-10-23: the voyage folder is gone, and a run's end record is `<state_dir>/ended`. 6c-1c3 runs in two landings,
decided 2026-10-23. Pass 6c-1c3a landed 2026-10-23: each row's supervisor serves a held watch, and the wake's owed Enter
belongs to the run it was typed into. Pass 6c-1c3b landed 2026-10-23: the daemon holds a watch on each row, an
adopted row whose agent exits is restarted with no selection, and `ready` means the agent lived 2 s. Pass 6c-1 is
complete. 6c-2 runs in two landings, decided 2026-10-24 after its plan was checked against the code. Pass 6c-2a landed
2026-10-24: a row a 0.6 release left is retired before its first 0.7 start, and an end its person asked for is kept.
Pass 6c-2b landed 2026-10-24: the window's dormant attach-only drawer, rows' drawer arm and messaging's voyage handle
form are gone, and 6c-2 is complete. 6d runs in three landings, decided 2026-10-24 after its plan was checked against
the code. Pass 6d-1 landed 2026-10-24: the local connection client, the shared test support and the log masking are
platform's. Pass 6d-2 landed 2026-10-24: the capsule is the crate `sot-capsule` in `rust/capsule/`. Pass 6d-3 landed
2026-10-24: the capsule's contract is settled, and the capsule series (D2) is complete. The publish guard's landing A
landed 2026-10-24: the guard is in the repository; each computer's clones migrate to it on the maintainer's word. Its
landing B landed 2026-10-25: `sotd install` enrolls the clones it knows and names them in its verdict. Pass 7
runs in 39 landings, decided 2026-10-25 after its plan was checked against the code. Pass 7a landed 2026-10-25: the window's row
listings and their canonical-id translation are `RowDirectory`'s, private. Pass 7b landed 2026-10-25: each row's label, root,
work state, flash, strip order and default slug are `RowPresentation`'s, private. Pass 7c landed 2026-10-25: the pending
results, their serials and the reveal are `ResultLedger`'s, private. Pass 7d landed 2026-10-25: the agent pane's
fields are `AgentPane`'s, private. Pass 7e landed 2026-10-25: the REPL's model and routing are `ReplModel`'s, private.
Pass 7f landed 2026-10-25: the REPL's presentation is `ReplPresentation`'s, private. The page channel's landing 1
landed 2026-10-25: a page the window opened reads row states, goes to a row and opens a row's decision page, through
verbs checked against its site's capability and origin; its trust boundary is this OS account, the maintainer's
decision. Pass 7g landed 2026-10-25: focus and the prompts are `FocusPrompts`'s, private, and no source scan guards
focus any more. The publish guard's history bound landed 2026-10-25: a new branch or tag from an enrolled clone scans
only what the destination does not hold. Pass 7h landed 2026-10-25: the drawer, maximize, wide preview and Help are
`LayoutHelp`'s, private. The page channel's landing 2a landed 2026-10-26: a page's reply is one line of at most 512
bytes, refused if it holds a control or direction character, marked `[sot-page] ` and offered to the page's window
(which drops it until landing 2b draws the bar); a confirmed reply is typed only at a free prompt, under the wake's
one attempt per row (the change 22 minutes, launch to main 84: the first test run's wake suite took 10 minutes, the
hosted wake shard overran its bound and the new tests went to a shard of their own, and the gate's scratch clone
needed its own build folder and node packages). Whole-window landings 1 to 11 landed 2026-10-26: the twelve-owner
milestone and navigation, the reveal walk, file operations and the drift check are owners, private. Since 2026-10-26
(the maintainer's decision) a landing is proved before main by the local gate, the build and the existing suite; the
hosted Rust and CI runs test main after the merge, one run per batch of merges, none waited for before the next merge;
a red post-merge run is fixed forward in the next landing or the landing is reverted; the native window run stays once
per batch after the merge. Whole-window landings 12 to 30 landed 2026-10-26, and the window's `State` has 104 fields
left of 236. The installer's crossing words landed 2026-10-26: at the 0.6-to-0.7 update it says every agent ends once
and its conversation comes back by resume, and the install guides say what to do after an update (run again if an
update pinned the install, restart every window, a window's computer runs its own daemon, where `sotd` is). The
unbuilt exported types `Mode`, `Tool` and `Capture` left `core/` on 2026-10-26, before the 0.7.0 tag, as ADR 0051
decides; `ConceptEntity` and `AnnotationKind` stay declared and unbuilt. The publish guard's speed landed 2026-10-26.
The page channel's landing 2b landed 2026-10-26: a page's offered reply is confirmed only on the window's
`ReplyBar`, by Enter after a second of the whole bar presented (any resize, cover or failed frame starts the second
again), keys follow what is drawn on top, nothing reaches a pane under a covered bar, and a reply whose answer is
lost reads unknown; the change about 100 minutes in all and the push 16 seconds, a miss: a fresh agent read its
200k read set cold, then three findings rounds on presentation, covering layers and the status line, which an agent
holding the window would have known (the captain's rule since: a change to a subsystem's behavior goes to an agent
holding that subsystem whole). The crossing words and the core types' removal took 7 and 6 minutes of change, 37
minutes each from launch to main, and one push of 199 seconds, before the guard's speed. The link's first two
commits landed 2026-10-26: wire's link family and topology's link runtime, and `sotd stdio-bridge` as the far router
of a window's link; the change 26 minutes (18 and 8), the local gate 9, the push about 20 seconds, with 11 minutes
lost because the first commit's ready line went unwatched. The link landed whole 2026-10-26: the window reaches each
other computer over one login, the link gate, the lane child and the per-row and per-page logins are gone, and a
link's end ends every channel, every copy over it and the far router; the third commit's change 52 minutes, a miss
(48 files across the window, and a defect the keystroke timing suite found: the daemon's first-frame bound closes a
channel 0 that sends no hello, which ends the link), its one findings round 25 (the router's exit with its input open,
every copy cancelled at the link's end, a page's open bounded, opens in order), the gate 5 minutes. A real sshd, the
Windows login and the other-account tests run at the release gate. The publish guard's Windows fix landed 2026-10-26: main's Rust run was red on Windows from the guard's speed landing (16:20) until this merge, unnoticed for about four hours because no one watched main's runs after that merge; the faster scanner counted a blob's lines against its size, and Git for Windows' awk drops a CR before a line's end, so every push carrying a CRLF blob was refused; a CR now reaches the matcher as another non-word byte, proven by the branch's full hosted run on all three systems. The installer's 0.7.0 minimum landed 2026-10-26, which closes the tag blocker INSTALL-WIN-UNBOUNDED-STEPS: the process listing and the Windows end-state `--version` asks are bounded at 60 s, and a listing with no answer is never "none runs"; tests that drive the install's phases with a cut listing wait for 0.7.1. The observation effort behind it is one miss: about four and a half hours of change across two agents and nine reviews (16:24 to 21:50), where the blocker needed about half an hour. Its class: each round bounded or read more of the machine than the blocker named, so each opened new answers to judge, first a new default at each call site, then hand-written second parsers of text our own subcommands print. The cure, our subcommands answering the installer as one JSON value of a shared type, waits on `pass/install-observe` for 0.7.1 with three named blockers. The batch's release-gate run 40448208255 on main at f771659ec is green whole: the native window jobs on Windows and macOS (the link's pane-timing route among them) and the Windows installer units and witness (64 minutes, the first real Windows installs with the bounded listing). The Stop hook's question line landed 2026-10-27 (9 minutes of change by a Codex edit job, main at 13:30): a SITREP-QUESTION line carries its options and recommendation, and a turn's first stop whose line lacks them is refused and stamps nothing, after the maintainer's Waiting on You page showed 8 of 10 cards without answer buttons. Direct page replies landed 2026-10-27: an opener grants one page entry a reply scope (`sot-fe docs --direct` a row's own page, `--direct-all` the two list pages, the open verb's page its own row; navigation and POST bodies never widen it); a scoped page's reply types at once or is held in memory until the row's prompt frees, a draft or a dialog refuses it, a row-scoped page answers only its own row, and the POST and `states.replies` answer sent, held, refused or unknown. Its change took about 24 minutes (the flag and scope edits 15 by a Codex job, the delivery 9 by hand) in one landing. It was first sized at three landings and three to four hours: the planner was asked for a load split, so it counted every contract whole as each landing's base (about 158k before code) and designed what the change did not need (a durable hold, question revisions, crash recovery); the change was four rules on the reply verb that 2a already had. A miss inside it: its first test run built no `sot-capsule` (a build line naming `--bins` twice), so the daemon suites ran twice. Main's hosted Rust run was then red from the card options (about 13:45) through the direct replies, unseen for about an hour: GitHub moved `ubuntu-latest` from 24.04 to 26.04 that afternoon, whose compiler fails the publish guard's bash 3.2 build and whose host fails the sotd shard's host-dependent tests. The Linux jobs are pinned to `ubuntu-24.04` (2026-10-27), and 26.04 support is queued. It went unseen because no watcher was armed on main's runs after either merge, and the one look read a six-run listing that cut the red run off; since, every merge to main arms one watcher on that commit's runs, found by its sha, until each run ends. The installer's state folder and checklist landed 2026-10-28, after new users on a server whose
homes are on NFS could create no session (the default state root sat in the network home, and the installer only
warned): a Linux daemon whose root does not qualify, by the row launcher's own check, gets `/scratch/<user>/sot-state`
and its drop-in unless the person chose a root, and every Unix run prints a checklist above its verdict. A miss: 31
minutes of edit (19 of a Codex job, 11 of a second round by hand) and 45 from the brief to main (16:16 to 17:01).
Its cause: one brief carried two features (the checklist was asked for while the brief was written) and five record
pages, and the Codex job could not build in its sandbox, so it worked blind and added four behaviors nobody asked for
(no `systemctl` read as no service, a failed restart as a stop, a new step for a daemon not running, the guard's
results merged last), which a reading had to undo; the second Codex round could not run, because the job runner
starts Codex ephemeral and a resume finds no session. Nothing is half-done on main. The v0.7.0-rc.5 cut (2026-10-29)
drafted nothing, a miss: that landing made a missing Claude Code a step of the checklist, the hosted runners have none,
and the install smoke takes only PASS, so 7 of its 13 legs ended ONE STEP LEFT (the Windows legs record no checklist
items, and the leg that installs the release before passed). No gate before a tag runs the install smoke, so the
release run was the first to see it. The fix gives the smoke a stand-in `claude` in its scratch home, proven against
rc.5's own Linux pack and by the branch's whole release run; it was on main 28 minutes after the failure showed (47
after the cut), and it cost one tag: rc.5 stays with no release, and rc.6 carries its product. Queued for distribution:
a landing that changes the installer's verdict runs no install smoke before the tag. rc.6 was cut 2026-10-29; its first
install, on the maintainer's workstation, ended ONE STEP LEFT on a false step (the installer judged the state folder
from its own environment, not from the unit's drop-in), fixed on main the same day. An rc.7 to carry that fix was
proposed within hours and withdrawn on the maintainer's question, "how can you be on rc7 when no one ever tried rc5 or
rc6?": each cut had become the test of the session's own installer change, with the session as the consumer and the
installer's verdict as the test. Since (the maintainer, 2026-10-29) a new candidate waits until a person has used the
last one, and carries everything that use found. The row tools seen in rc.6's first use landed 2026-10-30: a session
in a row knew neither that it ran in the window nor how to give its person a terminal of their own, read "none
attached" from inside an attached window, and was refused a shell row in its own folder with a worktree's advice. A
row's session start now says where it runs and how to make a terminal-only row (`comm-spawn.sh <folder> --agent
none`, then `sot-fe goto`), `comm-list.sh` asks its own daemon's `version.query` and says why it shows no window, and
the spawn counts the default row with no agent as holding no folder, the daemon's own rule. Four items took about 18
minutes of change and 30 from the maintainer's word to main (the gate 10). Pages, from rc.6's use, landed 2026-10-31:
with several windows up, the maintainer found at each computer he came back to tabs meant for the window he had been
at. A browser open goes to one window, the one a person used last, but the choice ranked only live windows, so while
his laptop's window was away (asleep or closed) every page fell to the live window used before it, however long ago.
The choice now ranks the windows away too: when the winner is away and no window on its computer is connected,
nothing opens and the session is told which computer's window is away (his choice of three rules). At his word the
same landing sends pages to his Queue instead of a tab: show-result opens no page and prints the `Page: <path>` line a
session ends its closing marker with, and the page channel reads the page of any stamp line, not only a question's.
A miss: about 55 minutes from his rule to main, three local gates and one Codex review (four of its six findings
fixed, two stated as limits). Its causes: the second feature joined the landing while the first gated, the first
gate failed because a skill text edit did not regenerate `comm/legacy-skills.txt`, and the second on a real-daemon
states test that pinned a summary with its page clause, each costing a gate of 10 to 15 minutes. A project chat in
the window was planned 2026-10-30 (Slack as its carrier, one listener per Slack workspace on the hub through a Ship of
Tools app of its own, a drawer per row with its history and figures both ways) and approved 2026-10-31. On
2026-11-01 the maintainer chose its route: the daemon speaks Slack itself, in Rust, with no chat child and no bot
package landing before it. Its plan, with its cost, went to him the same day, and he approved it, relayed by the
captain, at its fourth count (about 4,420 product and 6,730 test lines, preparations included), staged; the plan of
record is `docs/history/slack-client-plan.md`. Its web stack was measured and accepted (the branch that measured it
never lands), and the live Slack tests closed 2026-11-02 in his workspace: one workspace (a second is refused at load),
a person's post under a custom author with the app badge, and no text-prefix path. After the 0.7.0 tag the first stage
lands in this order, each piece alone within the half hour: P2, P4, P10, P11, P5, P6, P9, P1, then B1, B2, B5. Figures
(B4 with P3, P7, P8) and mail (B3 with P14, P13) wait for his word after the first stage is measured, and P14's lock
order is settled in its planning. SlackClaw's pause switch (its PR 18) merges on his word when the first channel moves.
A row's copy reaches the window's clipboard since
2026-10-31: Claude Code's `/copy`, or any program that copies by OSC 52, lands in the clipboard of the window showing
the row on every system (the terminal parser's bound on such a sequence rose from 1 KiB to 1 MiB, and one past it or
cancelled is dropped whole), and every copy and paste in the window goes through one clipboard handle kept for its
life, which the X11 clipboard on Linux needs. A page reply refused at a one-line draft quotes what the box holds
(seen in rc.6's use) since the same day. A send through the hub still reads "may still be filed" whenever a linked
box leaves the offer unanswered: a rule that judged by the handle's name which box could hold it was withdrawn before
main, since a row can be named with any box's name, and the fix waits for the cause of the silent link. Since
2026-11-01 the end-of-turn audit never orders a page badged (an `.html` page reaches its person through the closing
marker's `Page:`) nor a session's own handoff, after it had ordered both (seen in rc.6's and rc.7's use). Since the
same day the window draws a terminal cell's background and inverse video as a terminal does, after inverse text in
a row (nano's bars, Claude Code's highlights) showed as white blocks with no letters: an inverse cell had been drawn
as a block of its foreground, and every background was dropped. Since the same day a Windows session's PowerShell
call of a launcher (`sot-fe`, `show-result`, `ccb`, `sot-gh-auth`) answers at once that it runs from Git Bash, through a
`.cmd` beside each, after a `sot-fe version` from a session's PowerShell tool waited half an hour with no output right
after an install (the install was incidental: PowerShell cannot run a bash script). Since the same day a wait whose
line addresses its person ("your go") is refused, and a closing one is nudged once, to a blocked question or a wait
that names its session or job, after rows waited on the maintainer in purple for hours with no Queue card. Since the
same day `comm-list.sh` on a window's computer also lists the hub's rows by handle, after a session there, asked to
reach the hub's admiral, could list no hub handle, guessed one and mailed the wrong session. Since the same day the
first OS account a computer says hello to a daemon as holds that computer and is still served, and only a hello as
another account is refused, after one test hello that borrowed a computer's name under the hub's account locked that
computer's window out of the hub until the daemon was restarted. Since 2026-11-02 every attach's end in the window
names its stage, its cause and its full error, a row logs why it refused a connection, and the far router logs each
copy that ends in an error, after keys were lost during attaches in rc.7's use and the logs could not say why
connections closed about 100 ms after they opened. Since the same day the Queue shows a session blocked on another
computer whose window is connected, as a read-only card naming that computer, after a session waited 20 hours on a
laptop with no card. Since the same day a failed `loginctl enable-linger` is a step naming its command, and a missing
publish list is a checked detail, never a step, so a lab user's install no longer ends on a maintainer's tool. Since the
same day a PDF page press advances a target and one page request runs at a time, a PNG's print density gives no scale
bar, and a row id is unique within a process by count. Since the same day a Windows checkout whose owner is not the
account that will use it is never reported armed, after an install over an elevated ssh login said armed and the update
was dropped in silence, and a repeat install judges a connected window, not only the record. Since the same day a page
opened with reply rights and no named window needs person input in the last 30 minutes, the PDF plugin keeps rendered pages,
64 MiB at most, and a spawn no longer promises a join. Since the same day a hub holds one link per computer, its newest,
and a send that rests on a silent link also names the boxes with no link, after a send named only an unrelated silent box
while the handle's own box was not linked. Since the same day keys typed while a row connects are held in a line on
the pane and sent only by an Enter after the row's screen has shown, into the run it showed, after keys were lost
during attaches in rc.7's use. Since 2026-11-03 a row's Julia kernel starts on Windows: its load path was joined with
`:`, which Julia on Windows reads as one entry, so Julia-aware previews had failed at every start on every Windows
computer since 0.7. Since the same day, on Windows 10, the wake sends the line it typed when the console host repaints it
without its no-break space; a dialog drawn at the top of a tall pane (Claude Code's folder-trust dialog among them) and
a row whose Claude Code is not logged in read red, joined or not; only `--version` or `--archive` pins an install, never
the tag an older installer hands off; `sot-fe version` says a computer's link is its mail link; the owner correction of
an elevated Windows install runs on a hosted Windows runner at every landing; and a microscope image's scale bar is
proven from its real header to the window. Since the same day (the maintainer's decision) a window lease that ends
with no intent, a crash or a kill, is a handover: the sessions wait one minute for a window to return, and only a
deliberate close, which now always sends its intent, ends them at once, after a Windows window panicked 16 s into a
REPL run and every session on its computer ended although a new window started 21 s later; the Windows launcher keeps
a window's output across its next start (`frontend.stdout.prev.log`, `frontend.stderr.prev.log`), since the panic's
message was lost; and every landing shard links its test programs before its timed step, after a macOS runner at half
speed ran out of its five minutes compiling, with no test failed. v0.7.0-rc.8 was cut 2026-11-03 from these. Since
2026-11-04 a texture the GPU cannot hold is refused with an error, never a window panic, and an inline math raster is
never wider than the preview pane, after a Windows window on 0.7.0-rc.7 panicked twice in one morning on a
100,748-pixel-wide math texture from a markdown preview full of dollar signs (the kept window output named it). Since
2026-11-05 a row's open question outlives its session, after a host restart started every row's agent afresh and
every red card left the person's Queue: the leg's bootstrap prompt had counted as a person's, which answers a
question, and the join wrote the whole registry entry; the bootstrap is now a machine turn, a join of the same row
keeps its question, and the session start names it. Since the same day the end-of-turn audit counts a markdown file as
a result only when the closing text names it, after it ordered a session's own working queue badged into the window
twice. Since the same day the installer's comm step loads ShipTools with `--compiled-modules=no`, after an elevated
Windows install failed there: Julia's loader stat'd the source path a ShipTools cache had recorded through the
person's `repo\current` junction, which an elevated process cannot traverse, and threw instead of skipping the cache
(reproduced on that computer; with caches off the module loads in 0.4 s; proven when 0.7.0's install over the same
elevated login passed its comm step). Since the same day the spawn's and the
despawn's lines (`sot_say`) are written by a program, not the shell's builtin, after the release gate's macOS bash 3.2
job failed the despawn test whose caller stops reading: the builtin's refused write stayed in the shell's output
buffer, and every later command substitution captured it (proven by the release gate's macOS job on 0.7.0's code).
Since the same day every folder-trust preparation is one log line naming its outcome, a recorded or already-trusted
entry at info with the trust file, after a move of 23 rows to another account left 3 at Claude Code's trust dialog
and the log could not say whether their entries had been written (the restart that recovers such a row is planned,
not built). Since the same day every step of rust.yml's build+test job has its own bound and the job's ceiling is
their sum, after a macOS runner at a quarter speed ran out a job's ceiling with every step green. Since the same day
(the maintainer's request) the session strip's order is a choice, cycled by `Primary+Shift+O` and kept across
restarts: by state (the default, as before), alphabetical, or by recent work-state change, each within its
computer's block. Since the same day a row's PDF page comes back from a row switch with its page and count, after the
maintainer found a PDF "terrible" and deaf to PageDown on 0.7.0-rc.8: the parked view kept the page's bytes but not
its page, so the restore drew it with the standalone raster's nearest sampler and the paging keys were not admitted.
v0.7.0-rc.9 was cut 2026-11-05 from these and installed from its draft on two of the maintainer's computers, where
he saw no issue in use; v0.7.0 was cut from the same code and published the same day on his word, and a plain install
(no version named) moves a computer to it and ends its rc pin. main carries 0.7.1-DEV. The chat's preparation P2
landed 2026-11-06: wire's `op_table!` declares each op once and gives `op::ALL`, so a declared op joins the first-frame
refusal test with no list to keep, and no test reads the op table's source. Its P4 landed the same day: a Slack
token is masked whole, by its form, in every log written through `RedactingWriter`, and no `RUST_LOG` setting lets a
network library's own diagnostics into the daemon's log. Its P10 landed the same day: the daemon's services are one
value, `Services`, built once in `run`, in place of 17 to 19 positional parameters; each handler is still given only
what it uses. Its P11 landed the same day: a controlled end retires the in-process services registered with lifecycle,
admission closed, stop asked and completion awaited within 1 s of the terminal's unchanged budget, and one that does
not finish is named and never changes the exit code; nothing is registered until the chat lands. Its P5 landed the
same day: a connection that falls behind a bus gets one typed `bus.lost` frame naming the bus and the count, which an
older window or linked daemon ignores; no pane recovers from it yet, and the chat is its first consumer.
- Findings go to `docs/defects.md`, under the owning subsystem, and are handled at that subsystem's pass.

## Landing records since 2026-11-06

A landing's record goes here, with its measured times; the package page holds only the current state.

- Install check, 2026-11-06: a whole gate run, and a scoped run that selects the shell scripts piece, build a Linux pack from the gate's own binaries and run the install smoke's `fresh-local` against it, about 2 minutes on a warm depot; with the smoke's stand-in `claude` removed the job fails on `ONE STEP LEFT`, the failure that lost rc.5. Goal-only brief; a fresh review for 4 minutes found the job had no depot of the gate's, so a real run would have resolved the Julia environments cold over the network. A miss past 45 minutes: 29 minutes of change, 15 of them a run hung in a Julia download, then the wait for the gate files the defect list held.
- Defect list, 2026-11-06: `docs/defects.md` is the one list of open items by subsystem, each marked person, session, note or feature, and every gate run fails on a landing record naming an item it does not hold or a contract citing it with no item. The contracts' queue sections moved into it. A miss at 39 minutes: 17 of fold, the contracts' queues a source nobody had named, 5 of review with one blocker, 4 of fixes and a second whole gate of 9.

- P1, 2026-11-06: mouse selection over flowed text is `ui/flowed_selection.rs`'s, private; a pane lends its buffer, rect, origin, clip and media placeholder per call, and the preview is the first consumer. A fixture pane that is not the preview is tested against the preview's copied text and clipped rectangles. 5 minutes of change, goal-only, then a fresh reviewer for 24 minutes (no blocker; an eager walk, two untested conditions and a false comment) and 3 minutes of fixes: a miss against the half hour, the review's length its cause.
- Scoped gate, doc tests, 2026-11-06: a scoped run asks `--doc` only of a touched crate with a library target (`src/lib.rs` or a `[lib]` table), never from a list; with none such it has no doc job and no doc result, after `no library targets found` failed the binary-only `sot-frontend` and `sot-backend`.
- The chat's preparations, 2026-11-06, change then launch to main in minutes. P2, 18 and 29, from a full brief on Haiku,
  about half the change compiling; the brief named one wrong function and one check that fails at the base. P4, 3 and
  18, from a full brief on Sonnet; the same brief on Haiku had died 3 minutes in, its window refilled to the compaction
  point by the 45K tokens every agent then loaded, and each of those minutes counts. P10, 6 and 20, from a full brief on
  Sonnet, 88 net lines deleted. P11, 5 plus a 2-minute review round for wall-clock margins, from a goal-only brief with
  one agent planning and editing. P5, 6, goal-only; the session's review of its wire change added the ownership row.
- The package page's trim, 2026-11-06: 67,806 bytes to 15,506, 3 minutes of change, goal-only; every State sentence
  checked present in this file, and one false clause fixed in review.
- The scoped gate, 2026-11-06: `RCG_SCOPE=<base>` narrows test jobs by one rule (`scripts/CLAUDE.md`), 6 minutes of change
  plus 3 for a review round that made a Rust path add the shell or Julia suites that prove it, goal-only. Under four
  minutes is its test, read at the first scoped landing after it.
- P6, unsupported operations, 2026-11-06: the daemon's generic unknown-op answer to an op (`{"error": "unknown op: <op>"}`,
  nothing else) ends only that request as outcome `unsupported`, named by no op (`ops::is_unsupported`); `page.reply` is
  the first user, and any other reply stays an error (fe-net's contract). 4 minutes of change, goal-only.
- P9, connection-scoped mutations, 2026-11-06: a request kind says whether it is replayable (`OutgoingReq::replayable`);
  one that is not (`page.reply`, `repl.eval`, `repl.run_file`) is ended, with its kind's connection-ended outcome and
  nothing written, if it still waits when a connection ends or when the next one is about to send its hello, so it is
  written on one connection at most (`Outbox::retire`, fe-net's contract).
- B1a, the chat crate, the wire record and normalization, 2026-11-06: 11:49 to main 12:20, 31 minutes, a miss by one. About 3
  to read the plan and make the worktree, 6.5 of change from a goal-only brief, 8.5 of review (no blocker, five should-fix), 10
  of whole gate, passed the first time. Two proofs the brief asked for did not hold as written: `cargo fmt --all --check` and
  strict clippy already fail on main in code this change does not touch, so a brief asks for the gate's own lint line.
- B1a's review follow-up, 2026-11-06: 12:09 to main 12:22, 13 minutes. 2 of change, 6 of review (one records blocker), under 1
  of second round, then the first clean scoped gate: 50 s wall, whole lint, the Windows and macOS checks, and the chat crate's
  15 tests. That is the scoped gate's test, a fix inside one piece gated in under four minutes. Text no longer decides a post's
  origin, and the file-share label comes back with outgoing files in B4.
- B1b, the web stack, the HTTP core and the fake Slack, 2026-11-06: 12:25 to main 13:03, 38 minutes, a miss, stopped and written
  up here. 14 of change, then 10 of review beside a gate started at once (no blocker, four should-fix). That gate failed at 12:51
  on CAPSULE-RESUME-TEST-UNDER-LOAD at load 73, in a test this change does not touch; the one rerun passed in 9 at peak load 15.7,
  and without it the landing was 26 minutes. 37 new lock entries, no banned crate. The gate's macOS check passes only because
  nothing links `sot-chat`: on Linux ring cannot compile C for its macOS target, which must be settled before B2 links the crate.
- B1b's review follow-up, 2026-11-06: about 12:50 to main 13:05, 15 minutes. 3 of change, 8 of review (one blocker: the child's
  proof of running was a predictable file in the shared temp folder, now its exit code), 2 of second round, scoped gate 54 s.
- B1b's hosted red, 2026-11-06: `chat_operations_keep_bounds_after_caller_drop` failed on Windows on 21cc2c2d0 and 7a537a17b, a 1 MiB
  reply hanging to its 10 s deadline. Cause, proven red on Linux: the fake's `Reply::Status` wrote with no flush, so tokio-rustls
  held output once the socket filled, and Linux's large loopback buffers hid it from both local gates. The fix and a permanent
  4 KiB send buffer in the fixture rode B1c, which held that file. The hosted result on Windows is read after this landing.
- B1c, the Slack web calls, 2026-11-06: 13:07 to main 14:10, 63 minutes, a miss, stopped and written up here. 16 of change including
  the fixture fix, 13 of `git commit` stuck on the shared drive at load 171, 25 of review (60 mutants, seven of eight priority rules
  pinned, one test blocker), 4 and 2 of fix rounds, 4 of a short review, three scoped gates at 49 to 67 s. Mutation found what the
  failing-first run could not: most tests failed at one shared setup call, so their red said little. The suite went from 51 s to 4 s
  by driving the name cache directly instead of 1,100 TLS handshakes. The last three test fixes took no further review: the reviewer
  had proved each kills its mutant, and a fifth mutation round would only find smaller gaps.
- B1d, the Socket Mode listener, 2026-11-06: 14:12 to main 15:00, 48 minutes, a miss, stopped and written up here. 19 of change
  against a twelve-minute target, 10 of review beside a 10-minute whole gate, which passed, then 5 of fix, 11 of second review and 9
  of a gate the lint file widened to whole. The first review stopped it on three blockers, two of them product defects no test had
  caught: each ping rebuilt the idle timeout, so the idle bound never fired, and a reconciliation during an outage cleared a gap
  nothing latched again. The reviewer's probe tests, written against what the guarantees claimed, found both; its ten mutants found
  only tests that did not pin a rule. The second review found no blocker. Its should-fixes are open: tests for a frame resetting the clock and for the hello latching over a held gap, a stale
  comment on `Timing::idle`, and the wording of the dial rule. 14 new lock entries, among them a second `webpki-roots` that the
  websocket feature pulls in and nothing uses at run time. The 30 s ping and 90 s idle are unobserved; a wrong value fails safe.
  C14 moved to B2, where the configuration and the service live. B1 is whole, without file transfer.
- B1d's review follow-up, 2026-11-06: 15:07 to main 15:25, 18 minutes. 3 of change, 10 of review beside a scoped gate that passed,
  3 of fix, 1 of the second gate. Six surviving mutants now fail at their own assertions; three tests are new and two are
  stricter. The review's probes found no product defect. Its one blocker was the test's own: a 390 ms bound on an interval that
  included the handshake, about 95 ms from failing a correct product on a slower host. The test now measures from the peer seeing
  the socket gone to the next open, which holds the rule without the fixed costs. Guarantee 19 now says any socket that ends.
- The hosted red on 6a669ae56, 2026-11-06: Linux's `rest-0+rest-1` job ran 304 s in a step bounded at 300, 295 s
  on the green run before it, with no test failing and the step cut off near its end; Windows and macOS passed. The job had 5.5 s
  of headroom and the next run grew 9.4 s across its binaries, 1.1 s of that in the chat crate. The run after it, on 2b80b78e3, took
  297 s and passed, so the job sat on the bound rather than past it. The fold moved, the bound stayed.
- The fold split, 2026-11-06: 15:26 to main 15:40, 14 minutes. One line of `.github/test-shards.txt`, a whole gate of 10 minutes and a
  short review, which stopped it on a wrong cause in the record: it had blamed the chat tests, and the two run logs showed no headroom
  instead. The corrected records took a scoped gate based on the whole-gated commit. Hosted, the widest Linux job is now 234 s.
- The ping landing, 2026-11-06: 15:30 to main 15:44, 14 minutes, first gate passed, review without a blocker. A live measurement
  replaced the two guessed numbers: Slack pings a quiet socket about every 10.2 s, answers ours, and drops a link that sends it
  nothing within a minute, as a bare end of stream. The listener already answered every ping under a stalled consumer and a full
  queue, so the tests are new and the one product change is the idle bound, 90 s to 45 s. The reviewer's probes bounded a held pong at
  the 5 s write bound; the one false sentence it found was the contract's, on what can hold a pong.
- The macOS check and the chat dependency, 2026-11-06: 15:40 to main 15:58, 18 minutes, first gate passed, whole, review without a
  blocker. `ring`'s build script now meets a stand-in C compiler in that one gate step, so `sot-backend` can depend on `sot-chat` and
  the step still type-checks macOS code; `cargo check` links nothing. The review showed the builder's word "links" was too strong: no
  daemon code names the crate, so `sotd` holds none of it, and the old-glibc pack and the Windows link have not met it yet. Their
  first proof is the first call, at B2e. Every manifest change of the stage is in this landing.
- The ping landing's follow-up, 2026-11-06: 15:43 to main 16:03, 20 minutes, two fix rounds, first gate at its head passed. Tests for
  the 5 s write bound and all eight product numbers, and a tighter pong window. The ordering check it was asked for could not catch a
  held ping, since the library flushes a queued pong before the next write; it stays, pinning that rule. The review stopped a helper
  that failed a correct product on a merely slow send and gave its flood frame and time caps, since Windows and macOS buffers are unmeasured.
- B2a, the chat configuration, 2026-11-06: 15:59 to main 16:15, 16 minutes, first gate passed, review without a blocker: 4 of change,
  1 of gate, 8 of review, 2 of records. 100 product lines were estimated and 341 landed, doc comments included. Its review's
  should-fixes rode a follow-up, whose probes then found an admitted token the log masker would cut short and print the rest of.
- B2a's follow-up, 2026-11-06: 16:15 to main 16:40, 25 minutes, three fix rounds. A planted FIFO could hang the load and a swapped or
  odd token was admitted; both are closed. The gate failed once, on a test's child that the all-targets lint refuses: the session had
  passed on a review's word that a lint allow was dead weight, and that review had read one of the gate's two lint lines.
- The one bounded read, 2026-11-06: 16:40 to main 17:24, 44 minutes, a miss, stopped and written up here. 13 of change, 6 of review, 5 of
  fix, 19 of gates. `sot_platform::host::read_regular` gives chat's configuration and `hosts.toml` one read that a FIFO cannot stall.
  The review's blocker was the goal itself: the daemon's store held a second reader of `hosts.toml`, unmoved, so the guarantee was
  false while every test passed. Cause of the miss: a path under `rust/platform` or `rust/topology`, a contract included, runs a
  scoped gate for 9 to 10 minutes, since everything rests on them, and this landing ran that gate twice after its first. A
  contract fix for those crates waits for the commit it fixes.
- B2b, the chat service, 2026-11-06: 17:31 to main 18:38, 67 minutes, a miss, stopped and written up here. 20 of change, then three
  rounds of review and fix (5 and 2, 4 and 1, 3), each gate about a minute and every one passed, and 25 minutes in which the
  second fix sat finished and unreported. `service.rs` takes the workspace's kernel lock before any call to Slack, keeps it in the
  listener's task so it is released only once no listener can run, checks the workspace `auth.test` reports, and refuses a second
  service with no network call. About 230 product lines landed against an estimate of 120, and 382 test lines. The three blockers:
  an API handle taken before stop still reached Slack after it, the contract's opening no longer said what chat owns, and a stop
  whose caller gave up carried the listener's task away, so the service stayed locked. Two were product defects that every test
  had passed, found by the reviewers' probes. Cause of the miss: three review rounds on a piece near twice its estimate, and a
  builder resumed by its id after the session moved accounts, whose report did not arrive and which no watch was following. A
  resumed agent gets a watch like a new one. Unproven and stated as a limit: a holder killed on one computer freeing the lock for
  another over the shared home; that check was refused by a safety check and did not run.
- B2c, the chat watch, 2026-11-06: 18:42 to main 19:24, 42 minutes, a miss, stopped and written up here. 6 of change, then three
  rounds of review and fix (6 and 3, 8 and 4, 9), every gate about a minute and every one passed. `watch.rs` keeps a watched
  channel's latest page true across live events, its snapshot and the socket's gaps: a bounded overlay while a snapshot loads, and
  a gap clears only once every watch has resynced from a snapshot begun after it. About 320 product lines across six files (249 in
  `watch.rs`) against an estimate of 100, and 354 test lines. Four blockers, three of them product defects every test had passed: an event queued before a snapshot
  began overwrote it and the gap cleared; a watch kept past its service took the next service's first gap as covered, since gap
  numbers restarted; and the first fix, a call order written into the contract, still lost an edit to a burst before the gap was
  read. The third fix moved the order into one call that takes the events and the gap under one lock. Cause of the miss: the
  guarantee's interleavings were found by the reviewers' probes one round at a time, and the first fix answered a race with a
  rule for the caller. A rule a caller must keep is a fix only when no call shape can hold it. As landed, a daemon with two event
  readers (a pump and a snapshot task) could still leave a watch complete and stale. Its follow-up, below, closed that.
- B2c's follow-up, 2026-11-06: 19:25 to main 20:07, 42 minutes, a miss. Every gate passed first time. Four review rounds, each
  finding one more interleaving of a pump, a snapshot and a gap: a stale contract sentence, then an older gap read rolling a
  shared watch back, then another stale sentence, until the fix made a watch's gap move forward only and one test ran all 4,200
  orders of a small scenario; the check's three wider enumerations then found nothing. `take` is private to the crate: a daemon
  reaches events only through a `Watches`, whose `begin` and `pump` each take and apply in one call, and waits on the service's
  `wait`, which a gap also wakes. Two `Watches` against one service stayed a stated limit. Cause of the miss: the enumeration
  came in the fourth round. For a guarantee about orderings, the first brief asks for the enumeration.
- B2d, the chat operations in wire, 2026-11-06: 20:14 to main 20:42, 28 minutes, first gate passed (scoped, 2 minutes), review
  without a blocker, run beside the gate. Requests `chat.resolve`, `watch`, `unwatch`, `history` and `post`, their replies and the
  events `changed` and `status` join the one op declaration, so the generated admission fixtures cover them; the record's bounds
  now hold on decode. About 300 product lines against an estimate of 125, and 230 test lines. The admission suite would not start
  `sotd` outside the gate's container, on this computer or on the gate host, at the base as well; the hosted Linux run passed it
  on this commit. That run was red on macOS, on a chat socket test this commit does not touch (its own record follows).
- B2d's follow-up, 2026-11-06: 20:42 to main 21:01, 19 minutes, two fix rounds and a third short look, none with a blocker,
  every gate passed first time. A chat request now ignores an unknown field, so a payload grows by defaulted fields both ways; a
  refusal decodes without a code, as an old daemon's unknown-op answer has none; a newer daemon's service state, watch state,
  resolve state, post outcome or record origin reads as a fallback, never as the app's own post and never as a post to resend; the
  record's bounds have one copy, in wire, which chat uses.
- B2c's second follow-up, 2026-11-06: 20:13 to main 21:39, 86 minutes, a miss, stopped and written up here. 20 of change, then
  three rounds of review and fix (blockers 2, 1, 0), every local gate about a minute and every one passed first time, with a copy
  onto main between rounds since B2d landed beside it. Main's hosted Windows run was then red: the enumeration ran past the test
  step's five minutes, since each of its zero-bound waits paid a timer tick, about 15 ms on Windows, across 16,800 runs. It was
  fixed forward by a zero-bound wait that never waits for a tick, a cost of the platform that no local gate could show. A `Service` hands out one `Watches`, which alone can wait (exclusively, and the
  wait says whether it woke, timed out or saw a stop); a stop or a dropped service ends the claim for good, so no channel reads
  complete over a stopped service; the enumeration of a pump, a snapshot and a gap now runs the product's own take and wake.
  The first two rounds each found one more way a stop or a wait met a kept watch: a second waiter that stole the pump's wake and left a watch
  complete over a gap, then a watch kept past its service and only ever called against the next. Cause of the miss: the
  enumeration was asked of the pump, the snapshot and the gap, not of every public call. For a guarantee about orderings, the
  enumeration's steps are every public call, a stop, a wait and a drop included.
- The hosted red on cf029b92f, 2026-11-06: macOS failed `a_chatty_peer_that_never_answers_keeps_its_link` (a second link opened)
  in chat code that had passed every run before. Fix started 20:55, main 21:58, 63 minutes, a miss: 15 of diagnosis and change,
  two review rounds with no blocker, then a wait for the change beside it to land, since both touched the socket tests. The cause
  was proven by freezing the test process for 0.7 s: on resume the idle timer fires before the paused peer's next frame is read,
  and the client retires the link, as the idle rule says; CPU load alone never reproduced it. Every test that holds a bound on a
  measured duration now runs beside a ticker and is judged only on an attempt with no pause as long as its margin; the review
  found the reconnect-pacing test more exposed than the one that went red, which is why the class was swept in a second round.
  The product's behavior under a stall is a stated limit. Load found a second, unrelated failure, CHAT-FLOOD-CAP-UNDER-LOAD.
- The selection requirement, 2026-11-06: 22:26 to main 22:30, 4 minutes. The maintainer's answer to the selection plan (every
  recommendation taken) put its requirement into `requirements.md` in the words he approved, checked against them by a script;
  no reviewer read it, since the text was his. Scoped gate one minute, passed first time.
- The selection op on the wire, 2026-11-06: 22:26 to main 22:59, 32 minutes, a miss, timed from its builder's launch (first
  recorded from 22:31, which was not the start). `selection.set`, its reply and the one
  selection digest, every payload bounded at decode, built beside the chat's hub piece in another worktree. 100 product and 110
  test lines. The first scoped gate passed in two minutes; the review found one blocker, a test gap: the digest was compared only
  with itself, so a version hashing one byte per character passed, and fixed vectors now pin it to the UTF-8 bytes. A second
  review passed it, and the gate passed again.
- The secret scan's allowlist for a chat test fixture, 2026-11-06: 23:14 to main 23:31, 17 minutes. Main's hosted secret scan
  went red on the selection op's merge, from a made-up Slack app token in the chat hub piece's tests: the scan reads every
  pushed branch, and four unmerged branches of that piece carried it. Deleting those branches was refused as destructive and
  stays with the maintainer, so the value is allowed by its exact text under the app-token rule only, as the other test
  fixtures are; a full-history scan then found nothing. The same run's documentation job failed on a network reset during
  `npm install`, and its rerun was requested. The scan's timing is SECRET-SCAN-AFTER-PUSH.
- The chat hub piece, 2026-11-06: 22:26 to main 23:44, 78 minutes, a miss, stopped and written up here. `sotd` builds the chat
  service once with its other services, only on a daemon positively identified as the configured hub (topology's current load,
  decided before the chat configuration loads), and retires it through lifecycle; a non-hub opens no secret file. About 330
  product and 500 test lines. The first gate passed in 11 minutes before review, and each later gate passed first time. Three
  review rounds (blockers 1, 1, 0): the first found the hub rule untested against a listed non-hub and a retirement arriving
  between registering and starting lost, a product defect; the second found that the 400-try race test written for that fix
  could not fail while the contract said it could; the third, on a stated scope, checked the order pinned by a test-only hook.
  A made-up token in its tests meanwhile turned main's secret scan red through its pushed branches. Cause of the miss: the
  brief did not ask for a deterministic seam where two threads race. The release workflow, dispatched on its head (builds and
  smokes, no tag, no draft), passed whole: the static musl Linux build compiled the chat crate into `sotd`, the daemon booted
  and answered hello in an Ubuntu 20.04 container (glibc 2.31), and the Windows MSVC build passed with its artifact smoke and
  four install smokes, as did macOS. The first link of `sot-chat` into the daemon is proven on every release path.
- The selection check and its rendering, 2026-11-06: 23:50 to main 00:32 on 11-07, 41 minutes, a miss. Built in 7 minutes
  beside the chat's recheck in another worktree; about 145 product and 123 test lines, and nothing calls them yet. The first
  review's probes found the code right on every input and two rules no test pinned: the guard refusing a range whose start is
  not before its end (without it a reversed range panicked) and a 9,000 cap no input could reach, now an invariant the compiler
  proves to the unit. A file name's control characters now show as a question mark, and the truncation note counts characters
  while the cut counts UTF-16 units. The second review found no blocker; its two should-fixes ride the handler piece.
- The chat recheck, 2026-11-06: 23:50 to main 00:32 on 11-07, 41 minutes, a miss. Every chat action passes one admission that
  reads the row's current entry and the hub's mapping when it acts, so a removed or replaced row, the inert anchor and a pair
  the mapping no longer names reach no service; resolve joins the mapping with the service's status; unwatch has its own door
  that hands out nothing able to reach Slack. About 85 product and 250 test lines. The first review found no blocker and four
  should-fixes it made a condition of landing, among them an exempt action that handed the whole service past the recheck; the
  second found none. M3 is proven at the adapter; end to end it waits for dispatch. Cause of both misses: two review rounds of
  about ten minutes each and a ten-minute gate run in series, and a gate on the first versions, started before the reviews
  ended, held the gate host five minutes after both fixes were ready. The two landed together on one gate of the stacked
  changes.
- The fake Slack for the backend's tests, 2026-11-07: 00:41 to main 01:42, 61 minutes, a miss. `sot-chat`'s `test-support`
  feature exposes the fake Slack and `Http::for_test` to other crates' tests, and `sot-backend` enables it as a dev-dependency
  only. Two guards keep it out of a shipped build: a check the gate runs on every run resolves the workspace's release build
  for the release targets and fails when the feature is on, and the feature refuses to compile without debug assertions,
  whatever spelling turned it on. About 20 product, 90 script and 15 test lines, built in 7 minutes. Three review rounds
  (blockers 4, 2, 0); every blocker was in a text search of the release files for feature flags, or in the gate's pin of the
  check's self-proof, never in the resolution check; the captain was told before round 3, which replaced the search with the
  compile error. The first whole gate failed on the repo's portable-sed lint; the next two passed. Cause of the miss: the brief
  asked that every path to the feature be caught, closed or named, and the command-line path was caught with a text pattern
  that each round beat with a new spelling; a guard that reads no text closed it.
- The chat request ops, 2026-11-07: 01:45 to main 02:52, 67 minutes, a miss. `chat.resolve`, `chat.history` and `chat.post` run
  as jobs off the connection loop, their admission on the blocking pool, so a stalled Slack reply delays no ping or other op
  (M4); each passes the one admission, so a removed, replaced, remapped or anchor row makes no call on the fake Slack (M3 end to
  end); a post is sent once; `chat.watch` and `chat.unwatch` answer `unavailable` until their subscriptions land. About 190
  product and 330 test lines. The builder stopped at three minutes on a premise of the brief, taken from a review note and never
  run: the service's Slack address is fixed, so the backend could not reach the fake; one start seam taking a base URL was then
  exported under `test-support`, held out of shipped builds by the same guards. Every whole gate passed first time (three).
  Three review rounds (blockers 3, 1, 0): round 1 found no product defect and three false or stale record sentences; round 2
  found that the contract said all three ops run off the loop while the test held only a history, so `chat.post` placed on the
  loop left every server test green; the captain was told before round 3, which pinned the three placements and made the
  error-to-code map name every case. The export's leftover pin rode it: the release-features check's self-proof now shows the
  compile error. Cause of the miss: a brief premise no one had run, and a guarantee sentence over three ops with a test for one.
- The chat watches, 2026-11-07: 02:57 to main 05:27, 150 minutes, a miss. `chat.watch` and `chat.unwatch` run in the
  connection's loop, so the reply to a watch is written before the loop reads another chat event; one owner task holds the
  service's one `Watches`, runs each snapshot off the loop and routes `chat.changed` and `chat.status` to the connections that
  watch. A connection's watches end with it through a drop on its side, an unwatch ends only that connection's watch, every
  snapshot admits every watch on its channel again and releases each whose row went away, a busy snapshot is retried with the
  watch kept, and a stop reaches every watcher as `chat.status`. About 550 product and 770 test lines against an estimate of
  150 and a ceiling of 300 product lines; the builder did not stop at the ceiling, and the change landed whole, since a split
  after the build adds a gate and a landing. Built in 37 minutes; every scoped gate passed first time (three). Two review rounds
  (blockers 2, 0): round 1 found one product defect, a snapshot that stopped at the first watch with a current row, so a stale
  watch behind it kept its slot and its events, plus a stale ownership row and eight rules no test pinned; round 2 probed every
  order of three watchers and found no stale watch kept, and its one should-fix, a stop test whose idle-connection check read
  past events, rode a last two-line commit with the 32-channel tests' waits lengthened after they failed under heavy load.
  Cause of the miss: an estimate off nearly fourfold for a piece with ten guarantees, and two reviews of about half an hour
  each in series with a half-hour fix round.
- The chat watch tests' hosted red, fixed forward, 2026-11-07: 05:36 to main 06:13, 37 minutes, a miss. Main's hosted run on the
  chat watches failed the two 32-channel watch tests on Windows and macOS at their 30-second wait, while hosted Linux passed them
  in about a second. The cause was the test, not the product: its many-channel helper returned before the fake Slack socket's
  hello, and every hello latches a gap, so a snapshot begun or completed before it is told to resync; when the hello landed
  among the 31 watch requests, each later request began the flipped channels again, the fake saw 35 to 40 snapshots against 33,
  and the test's exact count was passed and never matched. The helper now waits for the hello, as the single-channel rig did,
  and the waits ask for each channel's complete state. Test lines only (12 added, 4 removed). The builder reproduced it at 7 of
  12 copies pinned to two CPUs and showed 600 clean runs after; the review confirmed by ablation that the hello wait alone is
  the cure (the complete-state waits alone still failed 15 of 36) and that the daemon tells a watch begun before the hello to
  resync. The last landing's longer wait had been a deadline raise on an unmeasured reading of CPU starvation. Scoped gate and
  a hosted run on the branch (43 jobs) passed first time; one review round, no blocker. Cause of the miss: a hosted run, a
  review and the gate in series after a 13-minute diagnosis.
- The watch tests' follow-up, 2026-11-07: 06:14 to main 06:40, 26 minutes, inside. Two tests pin the ordinary start of a
  daemon, a watch begun before the socket's hello: one whose snapshot completed before it and one whose snapshot was in flight
  across it are each told `chat.status` with the gap and the channel, and a repeat watch completes the channel; a semaphore in
  the tests' levers holds the hello. The complete-state wait and the hello wait are each one helper, which names its call site
  on a timeout, and the server contract's table names both tests. About 77 test lines added and 19 removed, no product change.
  Both scoped gates passed first time; one review round, no blocker; its two should-fixes rode the landing unreviewed (eight
  lines, the reviewer's own).
- The selection handler's first part, 2026-11-07: 06:44 to main 07:31, 47 minutes, a miss. The daemon serves `selection.set` on
  the connection loop: it resolves the workspace to its row and the node through the preview's resolver (so a link out of the
  root is followed as the preview follows it), reads at most the preview's 2 MiB of a regular file, checks and renders it, and
  publishes the row's `selection.json` whole or removes it; a clear, every refusal and a failed publish leave no file, and a
  removal that itself fails answers `publish_failed` with the old file kept. An accepted selection answers `held` until a leg
  carries the hook. The planned piece was split before launch: the replies that tell the window who will hear went to the next
  piece and the map of who set each file to the withdrawal piece. About 100 product and 205 test lines. The builder stopped
  at once on the session's brief, which listed an out-of-root link as a refusal the plan's resolver does not make; the plan
  stood. Two review rounds (blockers 2, 1): round 1 found no product defect, a failed removal and the regular-file guard
  unpinned and one contract sentence false; round 2 closed both by mutants, fuzzed the new strip of the root and found it
  cannot panic, and found one contract sentence false, which the session corrected without a third round. The first two
  scoped gates passed; the third, on the contract fix alone, failed one chat test no commit of the landing touched
  (CHAT-STOP-STATUS-UNDER-LOAD), and the landing went in on the gate of the byte-identical code. Cause of the miss: a brief
  premise of the session's own, two review rounds and twelve-minute gates in series.
- The selection replies, 2026-11-07: 07:38 to main 08:23, 45 minutes, a miss. The reply to `selection.set` tells the window
  who will hear: the handler's first check refuses `not_a_window` for any connection that is not a window's and changes
  nothing, before the state root and any row; a row whose agent kind takes no selection (one named list, `claude` and `codex`)
  is refused `no_agent` and its file removed; an accepted selection answers `shared` when the row's marker exists and `held`
  when it does not, and until the hook piece writes the marker every accepted selection answers `held`. The server holds one
  definition of a window (role `fe` with a non-empty name, the client's own declaration), set once from the hello on the
  connection's handler arguments; four earlier copies of the same rule call it, and the one that differs (the active window's
  lookup accepts an empty name) is stated. The relative path now comes from the files subsystem's own node-id rule. About 58
  product lines added and 20 removed, 96 test lines; the builder took 4 minutes. Two review rounds (blockers 1, 2): round 1
  found that `not_a_window` could never fire, since the plan's test for a window (a roster entry) holds for every control
  session and the table passed the flag as a literal; the fix put one definition in the server and pinned it by driving real
  connections. Round 2 found the product correct, a stale comment and an unpinned order, both fixed as it named them without
  a third round. All three scoped gates passed first time. Cause of the miss: a plan premise the brief carried unchecked, two
  review rounds and three ten-minute gates in series.
- The selection test on Windows, 2026-11-07: 08:25 to main 08:37, 12 minutes, inside. The hosted Windows job failed the
  selection handler's table at its absolute-path line, from the first part's landing on: the row's file is JSON, so a Windows
  path's backslashes are escaped and the test's search of the raw text for the path missed them; the exact rendering on the
  line before it passed. The row now reads the file as JSON, and a check renders a path with backslashes, which the old
  raw-text form fails on Linux. Test lines only (about 10), no product change. The scoped gate and the branch's hosted Windows
  shard passed first time; one review round, no blocker; its one should-fix, a contract clause about what the agent reads
  (the hook is unbuilt), was cut in the records commit.
- The selection withdrawal, 2026-11-07: 08:49 to main 09:47, 58 minutes, a miss. A row's selection file now names only a
  selection a live window connection of this process set, once the start-up walk is done. One process-wide map behind one lock
  holds each row's setting connection serial and file; the handler resolves and renders before taking the lock, which covers
  only the write and the map update. A connection's end (`ClientGuard::drop`, every exit of the connection included) removes
  the files its serial set and nothing another set; the start-up walk removes a row's file only when the map names no setter
  for it, so a share made before the walk reaches its row survives; a failed removal keeps its setter, so the setter's end or
  a later clear retries, and the next walk removes what is left. Connection serials became one process-wide counter, since
  the map's key must be unique across the process. The planned piece also held R11, the whole path through a test daemon; its
  half that reaches the agent's hook needs the hook, so it moved to that piece. About 105 product and 120 test lines; the
  builder took 6 minutes. Two review rounds (blockers 1, 0): round 1 found the suite intermittently red (18 of 150 repeats),
  since each test roster started its serials at 1 and a test connection's end removed other tests' files; the cause was
  twofold (the per-roster counter and the tests' own constant serials), measured at 20 of 150 before and 0 of 160 after.
  Round 2 repeated the filter 300 times clean. Both scoped gates passed first time (the first on luck, given the red the
  review found). Cause of the miss: two review rounds and two ten-minute gates in series after a build of 6 minutes.
- From here on (2026-11-07 10:44) a landing's time is recorded as where its minutes went, with no half-hour mark and no miss;
  the maintainer's standard that no code change takes an hour stays.
- The selection variable, 2026-11-07: builder launched 10:06, main 10:49 (5f73bd0f8), 43 minutes. Every agent leg a row's
  supervisor starts carries `SOT_SELECTION_FILE`, its row's state folder plus `selection.json`, in a form a Git Bash shell
  reads on Windows (forward slashes, a verbatim prefix removed by platform's `simplify_verbatim`); no nested leg inherits it:
  the variable is on the nesting scrub list that `sotd agent-exec` and a row spawned inside a row apply, and `ccx` drops it
  unless started with `--capsule`, which the daemon passes for a Codex row's own leg. The spawn's scrub-then-row-pairs order
  is one function a test pins; a real daemon's session test pins the folder. Before the first commit, the plan's premise
  that Claude Code shows a prompt hook's message and passes its context to the model was run on Linux (2.1.319): it held,
  a message queued during a turn runs the hook when it is sent, and a cleared selection stays in that conversation's history.
  About 50 product and 75 test lines. Where the minutes went: the build 5 (10:06 to 10:11, beside the premise run); three
  review rounds of 8, 10 and 7 minutes (blockers 1, 0, 0); three fix rounds of 2, 1 and 1 minutes; three scoped gates of 8,
  10 and 10 minutes, each beside its review; one hosted branch run beside the third, for the Windows-only test. Round 1
  found the plan's premise false that `ccx` is only a nested launcher: it is also a Codex row's own leg, so the first
  version stripped the variable from every Codex row; the fix pins both ways by running the real `ccx`. Round 2 found the
  real-daemon assertion composing its own path while the leg gets the canonical state folder, which on Windows is verbatim
  and would have given `//?/C:/...`; a third round, bounded to that fix, ran on the session's decision. Its two text
  should-fixes ride the next agents change.
- The session start after a clear, 2026-11-07: builder launched 10:51, main 11:36 (cfa67f9ab), 45 minutes. A regression
  seen by the maintainer: the hook entries that ran the comm session start after `/clear` and a compaction were lost when the
  hooks moved into the launch recipe. Every Claude row leg's `--settings` now carries SessionStart on `clear` and `compact`;
  the hook runs a read-only session start that prints the handle, the line naming the handoff, the open question and the
  unread count, joins nothing and writes nothing, so the row's work-state survives a clear. It stands down when comm hooks
  are off, sends a refusal's reason to stdout and always exits 0; it lives with the registry and the installer ships it flat.
  A `/resume` typed in a live row runs nothing, and rows started before the update get it at their next start. About 50
  product and 150 test lines. Where the minutes went: the build 8; two review rounds of 16 and 13 minutes (blockers 1, 0);
  one fix round of 5; two scoped gates of 10, each beside its review. Round 1 found that the first form re-joined the
  session, and a join writes a fresh registry entry that keeps only the open question, so a compaction in mid-turn ended the
  row gray and dropped a stamped wait; the fix found the join records nothing a clear changes and made the start read-only.
  Three should-fixes are left for a later messaging change: the read-only start's tail repeats the full start's (two of its
  lines unpinned), its unread line takes the inbox lock, and its failure line reuses wording about sending.
- Destroying a session, 2026-11-07: builder launched 10:56, main 11:55 (fb209be81, 35bd32680, 201ba89b5), 59 minutes. Seen by
  the maintainer: destroying a session left the window in between. The window's own destroy bounced to the default row, and a
  destroy learned from a listing (another window, the command line, a row's end, another computer) never compared the list
  with the selection, so the view stayed on a session that no longer existed. One decision both paths call now moves the
  view to the next session in the Sessions list, below else above, across computers, and shows the no-session state with an
  empty pane only when no session remains; destroying another session changes nothing. About 230 product and 160 test
  lines. Where the minutes went: the build 6; three review rounds of 11, 12 and 14 minutes (blockers 1, 0, 0); fix rounds of
  4 and about 20 (the second added the move across computers, the session's decision, and a native fixture that runs only in
  the hosted release gate); two scoped gates of 2 minutes; moving the commits onto a main that had moved, about 3. Each round
  found real defects: the last session left its dead pane painted, two quick destroys could land on a row already gone, the
  wiring was unpinned, and a pending destroy's note lived too short or too long. The third round's six should-fixes are the
  follow-up below.
- Two tests that passed only on a slow run, 2026-11-07: builder launched 11:38, main 12:05 (f1738deba), 27 minutes. The gate
  is to run its test jobs 20 at a time; measured at 20, the quarto shutdown test left its stub sleeper alive past its end
  (the gate's leftover check failed) and the dropped-chat test read the listener's count before it fell to one. The sleeper
  now waits on a file the test removes, and the test asserts it gone; the chat test polls the count to one within its bound.
  Test lines only, about 13. Where the minutes went: the build 5; one review round of 21 minutes, no blocker, with 50-run
  loops under load; two scoped gates of 3 and 2 minutes, the second at 20 jobs. Under extreme starvation the quarto test
  failed 4 of 30 at its unchanged five-second start bound, a note. From this landing the gate runs 20 jobs at once, or 10
  when the build server's load is above 32 at the start, and its user-manager tests run beside the release gate.
- Destroying a session, the follow-up, 2026-11-07: builder launched 11:56, main 12:14 (6afa7e579), 18 minutes. A destroy's
  note ends when its row is known alive (a kept answer, a refusal, a lost connection to that computer); the pane is emptied
  before the view moves to a neighbor that must still attach, so the destroyed session's screen is never shown; the cleanup
  of rows a listing drops is keyed by computer and name as given; the manual and the window's contract say the order the
  code uses. About 90 product and 130 test lines. Where the minutes went: the build 4; one review round of 13 minutes, no
  blocker; two scoped gates of 2 minutes at 20 jobs (the second after moving the commit onto a main that had moved). Its
  three should-fixes are test pins: the pane clear before the move, a lost link clearing only its own computer's notes, and
  one native-fixture assertion that cannot fail. The two native fixtures run only in the hosted release gate.
- The conversation that held the seven landings above from the selection variable on (id 6113927f-2127-4aef-847a-70da25d024b3,
  10:00 to 12:15) ends here, at about 465K tokens. Besides the landings it ran the diagnosis of the trust entries lost at an
  account move (a measured lost update), two plans (the shared test tool with the test-run cut, and drawing in the preview
  pane, whose three questions wait on the maintainer) and the change of rules at 10:44 that dropped the half-hour mark.
- The bash 3.2 run covers what changed, 2026-11-07: builder launched 12:20, main 13:13 (d7f56463a, e7188007c, 2d0123583), 53
  minutes. The daemon test run is to come down toward the maintainer's four minutes; in a change that brought the shell
  suites, one serial job ran every hermetic suite again under bash 3.2, 509 to 532 s of the run. In a scoped run the parse and
  scan checks now run as one job when a file under the three shell folders differs, each selected suite runs under bash 3.2 as
  its own job when a non-markdown file there differs, and with neither nothing runs; a whole run emits all of them, and
  `under-bash.sh` with no option still runs everything in series for the release gate. About 75 script and 55 test lines.
  Where the minutes went: the build about 5; two whole gates of 251 and 249 s to test that the suites can run beside each
  other and their bash 3.2 twins (they could); two review rounds of 17 and 9 minutes (blockers 0, 0); fix rounds of 2 and 1
  (the second comments only); two more whole gates of 297 and 293 s. Round 1 found the suite order changed by a sort and the
  decision's use and both new modes unpinned; round 2 found two false comments and a guard, older than this change, that let a
  run pass with no bash 3.2 build: the follow-up below. A whole gate run took about 10 minutes before this landing and about
  4 to 5 after it.
- A selected bash 3.2 job never passes by being skipped, 2026-11-07: builder launched 13:13, main 13:38 (884716511, 0a0b733c5,
  f08b7973f), 25 minutes. The decision takes the build script's path, builds bash 3.2 itself and returns a failure naming the
  build when any job is selected and no bash 3.2.57 results; the main flow is one call that exits on it. About 15 script and
  45 test lines. Where the minutes went: the build 2; two review rounds of 8 and 5 minutes (blockers 0, 0); fix rounds of 2
  and 1; three whole gates of about 4 minutes. Round 1 found the code right and the tests weak (a text check missed a skipped
  call, a "builds nothing" case checked nothing, any bash was accepted); the tests now run the gate's own decision lines over a
  stub build that counts its calls, and only 3.2.57 passes.
- A test holds a thread at a marked line, in one process, 2026-11-07: builder launched 12:21, main 13:51 (a0226104e, 175fcc871,
  c37b8bbfd), 90 minutes, past the maintainer's hour. Platform's `hold_point!` and `hold_point_async!` mark a line; a test's
  arm holds arrivals by ticket until it releases them; every test-side wait is bounded at 30 s and a held arrival gives up
  after 60 s and fails the arm by name; no product-side path panics or refuses; a build without debug assertions evaluates no
  key and keeps no point name. One test-only dev-dependency line (tokio's test clock) in platform's manifest, the session's
  decision. About 420 product and 330 test lines. Where the minutes went: a premise stop of 4; the build 10; three review
  rounds of 16, 26 and 14 minutes (blockers 1, 1, 0); fix rounds of 8, 3 and 1; three platform gates of about 3 minutes; a
  hosted branch run for Windows and macOS; one move onto a main that had moved. Why past the hour: round 1 found a refusal
  inside a task gave a false green (the session then deleted the refusals that guessed a thread's kind from its name); round 2
  found that round 1's fix for a release-build warning broke the release compile of a keyed point, in a profile no gate
  builds. Round 3's should-fix and gap are the follow-up.
- Longest jobs first, 2026-11-07: builder launched 13:38, main 14:18 (f0f9164b1, 66f035393, b34caebc5, ceeb76f96), 40 minutes.
  The pipeline starts unmeasured jobs first, then the rest by their last measured time, longest first; times per target in
  `rc-gate-times.tsv`, keyed per job (a test binary by its target name, a shell suite by its path); the hand list `SLOW_FIRST`
  is gone. About 40 script and 80 test lines. Where the minutes went: the build 4; two review rounds of 13 and 10 minutes
  (blockers 1, 0); fix rounds of 4, 1 and 2; whole gates of 309 and 261 s (the first run on a target wrote an empty times
  file, round 1's blocker, seen on the build server), 331 and 236 s (the measurement: the first fully ordered run took 236 s
  against 249 to 261 unordered), and 237 s to land; the gate's own suites run by the session, since the local gate does not
  run them. One extra fix round came from the session committing while the builder still applied a follow-up note.
- A hold point has one form in both profiles, and a held ticket whose waiter is lost fails its arm, 2026-11-07: builder sent
  13:51, main 15:17 (91f3445f6, 6ffe29977, a7017b439), 86 minutes, past the maintainer's hour. Both profiles expand a point to
  `if ON { arrive(..) }` and each arrival function has one signature with a profile-dependent body, so no release stub can
  drift; a held async arrival waits on a blocking task, and a ticket still held when that task is lost (cancelled, or its
  runtime dropped) records its own state and fails the arm by name. About 60 product and 40 test lines net. Where the minutes
  went: the build 5; two review rounds of 17 and 25 minutes (blockers 1, 0); fix rounds of 5 and 3; three platform gates of
  3, 2 and 2 minutes; one move onto a main that had moved; the session's move to another account, which ended the builder
  (a new one took the last round). Why past the hour: round 1 found the gap this change existed to close still open (its new
  test passed on the old code too), and round 2 named three more fixes; a multi-thread runtime-drop test was deleted because a
  race in the runtime's shutdown decides it.
- A test holds a child process at a marked line, 2026-11-07: builder launched 15:21, main 16:35 (6 commits replayed as far as
  ee0c5b650), 74 minutes, past the maintainer's hour. A child started with `SOT_TEST_HOLDS` is held by the same arm, tickets
  and bounds as in one process: tickets numbered across every process sharing one folder by an exclusive create; an arm, a
  release, the arm's drop, a give-up and a lost waiter are files; nothing in the folder is deleted, so Windows never meets a
  file another process holds open. Every test daemon gets its own folder. About 300 product and 300 test lines. Where the
  minutes went: the build 16; two review rounds of 23 and 22 minutes (blockers 1, 1); fix rounds of 4 and 2; three gates of
  about 3 minutes; two hosted runs on the branch for Windows and macOS. Why past the hour: round 1 found a child whose wait
  was lost could pass its test, the same class the in-process follow-up had fixed; round 2 found that two of round 1's
  suggested deletions were load-bearing (the 60 s cap on a child's give-up, and the check that keeps arrivals after an arm's
  drop from writing files). Both are back, each with a fast test.
- A drawing has one shape, one bound and one raster, 2026-11-07: builder launched 15:56, main 17:01 (516b96652, a3cafa8ee and
  the contract's two sentences, replayed as far as dbc10f53a), 65 minutes, past the maintainer's hour. The first piece of the
  approved drawing requirement: `selection.set` carries an optional drawing beside the text selection, refused when both are
  set, empty or past 16384 points, and a request without it decodes as before; `sot_protocol::ink` rasterizes a drawing at no
  more than 2000 pixels on the long side, the same mask whatever order its segments are painted in, so the window and the
  daemon will draw the same ink. About 145 product and 200 test lines. Where the minutes went: the build 13; two review
  rounds of 25 and 15 minutes (blockers 1, 0); one fix round of 4; three gates of about 4 to 5 minutes. Why past the hour:
  round 1 found the raster painted each segment's whole bounding box, so a drawing at the wire's bound took 95 s; it now
  paints each row's span (about 5 s for random strokes, 13 s at worst) and a test counts the pixels visited. The stroke cap
  was deleted (the point bound holds it), and the contract states that a daemon built before this field reads a drawing with
  no selection as a clear.
- The conversation that held the nine landings above from the bash 3.2 run on (id 1f0479d4-c997-4b82-9118-7acfff5c4dc3, from
  12:17, moved to another account at 14:37 on the maintainer's word) meant to end here at about 17:10; its clear was refused,
  so it went on and holds the landings below too. By here it had taken the maintainer's answer on drawing (the requirement
  and its decisions are in), found that the local gate never runs its own suites, and found that a review's list of
  deletions is a claim to check: two such deletions were load-bearing.
- The chat watch's ordering hook is a hold point, 2026-11-07: builder launched 17:10, main 18:22 (7ae923a93, 1a84f1f6b), 72
  minutes, past the maintainer's hour. The chat watch's test-only closure is gone; its two ordering tests arm the shared hold
  point `chat.watch.between`, keyed by their own hub. About 15 product lines removed. Where the minutes went: the build 5;
  one review round of 10 minutes (blocker 0) and a comment fix of 1; two gates red on a chat stop test this change does not
  touch, and the wait for that test's fix (below); one gate of 2 minutes stacked on it. Why past the hour: the unrelated red.
- The chat stop test waits for the stop it asserts, 2026-11-07: diagnosis 17:19 to 17:58, builder launched 17:59, main 18:22
  (e08b1e4fc), 23 minutes from the builder. The test failed three gates in a row under load: lifecycle's retire returns at
  its deadline whether or not its step thread has asked the chat to stop, and the test read a report of incomplete as a stop
  already asked. Test lines only: a bounded wait for the stop to begin, and a second pass that forces the loaded order.
  Where the minutes went: the diagnosis 39 (it reproduced the red under load on a second build server); the build 5; one
  gate of 2 minutes; one review round of 14 minutes (blocker 0) beside the stacked gates.
- The durable write creates its temp file fresh, 2026-11-07: builder launched 17:37, main 18:23 (a3cc57126 and 7d5cbab5f,
  replayed as 5c2be032b), 46 minutes. Found while reviewing the drawing's picture: platform's durable write now removes any
  entry at its temp name and creates the temp with an exclusive create that never follows a link, for every caller of
  `write_durable`. About 15 product and 70 test lines. Where the minutes went: the build 4; one review round of 23 minutes
  (blocker 0) and a test pin of 2; gates of 2 and 4 minutes; a hosted run on Windows and macOS.
- The times of the eight entries below were corrected on 2026-11-08 from each brief's file time (the launch) and
  each landing commit's time (main). The step minutes first written here came from session notes whose clock ran 20 to
  80 minutes ahead, and are removed; the gate times are the gate's own.
- A child's file that a test polls for is renamed into place whole, 2026-11-07: builder launched 18:46, main 19:05
  (bf04197f6, fe6291036), 19 minutes. Main's hosted macOS run failed the hold tool's two-children test, which read a
  child's pass file between its create and its write (HOLD-CHILD-PASS-TORN); the window's exit-process fixture had the same
  shape. Both children now write a part name and rename it into place through `publish_noreplace`. Test lines only. Where
  the minutes went: the build and a second site; one review round (blocker 0) and its fix; gates of 2 and 2 minutes.
- A write beneath a root follows no link (Linux and macOS), 2026-11-07: builder launched 18:43, main 19:46 (bfbb8b524,
  56f7577d7, 7a566efb6, 1a46fdec1), 63 minutes, past the maintainer's hour. Platform's guarantee 17: `write_beneath` takes
  a root that is its own canonical path, opens it and each folder below relative to the one before without following a
  link, creates its temporary file exclusively under a fresh name, and renames and syncs within the same folder. No caller
  yet; the drawing's picture is the first. About 155 product and 230 test lines. Where the minutes went: the build and its
  red table; three review rounds (blockers 0, 0, 0) and three fix rounds; three gates of 2 minutes; three hosted runs on
  the branch beside them. Why past the hour: each round found a should-fix worth its
  time: the root's rule (first its spelling, then the canonical path), the folder syncs on macOS (now std's, which flushes
  the drive), a conversion moved before the first call, and the error a root that does not resolve answers. A hosted run
  of the next piece's two-writers test then found BENEATH-SUPERSEDED-WRITER-ERRS in this one.
- The endpoint gate's stubs are ended with their children, 2026-11-07: diagnosis 18:58 to 19:52, builder launched 19:52,
  main 20:12 (4ed49a71d, 61874b765), 20 minutes from the builder. Two local gates of the wake's change failed on two
  `sleep 30` processes alive at the census (GATE-LEFTOVER-ENDPOINT-STUBS); the diagnosis found them on every run of main's
  endpoint gate suite alone. Test lines only, and the stub pids renamed so the existing scan pins the fix. Where the
  minutes went: the diagnosis (on a second build server); the build; one review round (blocker 0) and its fix; gates of 2
  and 2 minutes. The review also found BASH32-RANGE-LOCALE and COMM-SUITES-BASH-CLAIM.
- The wake's test seams are hold points, 2026-11-07: builder launched 18:26, main 20:15 (4666d73ce, 5ed658e9b,
  43509908f), 109 minutes, far past the maintainer's hour. Two test variables a release build read are gone; the wake
  marks `comm.wake.hold`, `comm.wake.final_ok` and `comm.wake.done`, its tests arm them on their own folder, three fixed
  holds of 6 to 9 s became held arrivals, and the typing guard is released before `done`. About 25 product lines removed.
  The whole wake suite still takes about 635 s in one process: the absence sleeps are the next piece. Where the minutes
  went: the build; two review rounds (blockers 1, 0) with mutant runs on a second build server, and two fix rounds; four
  gates of 2 to 3 minutes, two of them failed by the endpoint suite's leftover above, whose
  diagnosis and landing it waited for. Why far past the hour: that leftover, and round 1's blocker: the run-fence test
  could pass under its mutant when the restart landed just after a dial of the attempt's client. It now waits the
  client's redial cap after the new run shows, a bound rather than a hold (WAKE-REATTACH-BOUND). Its second gate also
  failed the sotd unit tests once, with its log lost (GATE-KEEPS-NO-FAILED-LOGS).
- The write beneath a root keeps its rule while entries inside the root change, 2026-11-07: builder launched 19:49, main
  21:07 (3be61ccb9, 5e9a3a0c4, d79396550, 0cf4858d4, 53da9e1a3), 78 minutes, past the maintainer's hour. Tests change
  folders and names at each step of the write (a folder swapped before and after its open, one with another below it,
  one removed, a missing folder made and then changed, the temporary file replaced at its create and after its write, a
  link planted at the final name, two writers of one name); a step before a folder's make lets a test reach that
  ordering; the check after the rename is deleted, so Ok means the entry at the temporary name was renamed into place
  (BENEATH-SUPERSEDED-WRITER-ERRS). About 10 product lines and 290 test lines. Where the minutes went: the build; two
  review rounds (blockers 1, 0), the first with an addendum; three fix rounds; four gates of 2 minutes; two hosted runs
  on the branch. Why past the hour: round 1's blocker (the two-writers test red with no
  fault: the check's true error, and stale handles on a network home), then a reversal from keeping the check to
  deleting it once the reviewer showed it changed only the answer.
- The chat watch test removes its row while the owner's snapshot is held, 2026-11-07: diagnosis 20:27 to 20:52, builder
  launched 20:52, main 21:19 (585800d2f, e94a35239, 1944cb5c0), 27 minutes from the builder. Main's hosted macOS runs
  failed a chat watch dispatch test in 3 of 7 runs (CHAT-WATCH-STALE-SLOT): the owner's snapshot could check the row
  before the test removed it. The snapshot marks `chat.watch.snapshot` and the test removes the row while it is held.
  About 5 product and 9 test lines. Where the minutes went: the diagnosis (3 failures in 883 runs on a second build
  server, every run with a 300 ms delay); the build (500 runs, none failed); one review round (800 runs, blocker 0); gates
  of 2 and 2 minutes; one hosted run on the branch. The diagnosis also noted CHAT-TRANSIENT-WAITS.
- The wake tests count looks instead of sleeping, 2026-11-07: builder launched 20:25, main 21:53 (ddb045fd7, 75fb56788),
  88 minutes, past the maintainer's hour. The wake marks `comm.wake.looked` per row handle where a tick ends its
  decision; each absence test waits for two more looks of its row instead of a span of the clock; the stub writes a
  marker when its echo is shown; a unit test pins the repeat interval, which the old sleeps had caught. The whole wake
  suite went from about 635 s to 529 s in one process. About 10 product and 60 test lines. Where the minutes went: the
  build and the fix, each with mutant runs and a whole 9-minute wake suite on a second build server; two review rounds
  (blockers 1, 0); three gates of 2 minutes. Why past the hour: each builder round ran
  the slow suite whole, and round 1 found the repeat interval left unpinned by the conversion.
- The write beneath a root on Windows, 2026-11-07: builder launched 21:10, main 22:49 (eba799524 to 708bfcfc6, seven
  commits), 99 minutes, far past the maintainer's hour. Each folder is opened relative to the one before with
  `NtCreateFile`, an entry whose reparse tag is a name surrogate refused; the temporary file is created new, held against
  rename, and renamed by its handle within the folder; a relative root is refused before any call on every system; three
  `windows-sys` features added, no new crate. About 160 product and 200 test lines. Where the minutes went: the build;
  no Windows computer here has a Rust toolchain, so every Windows answer was a hosted round (four rounds, queue
  included); two review rounds (blockers 1, 0: a one-character final name was refused, its rename information shorter
  than the kernel requires) and four fix rounds; two sets of probes, each a fault on a scratch branch that must turn its
  named test red on Windows (the first set on whole matrices, cut short at the runner limit; the second on a one-job
  workflow, all eight red); one whole gate of 255 s. Why far past the hour: the hosted rounds.
- The agent's picture and the crop are written beneath the project root, 2026-11-07 to 08: builder launched 22:58, main
  00:36 (0f4c3dc44 and 1ae9109a7, the drawing's picture from its branch, then e15c894a7, 4b46319b9, f93a49dce, 73ef59e35),
  98 minutes, past the maintainer's hour. The daemon's picture (the figure with the person's ink, at
  `.sot/captures/ink-<row id>.png`) and the preview's crop write through `FilesMode::write`, which passes the workspace's
  canonical root to platform's write beneath a root; their sources open only a regular file, through platform's new
  `host::open_regular`, on which `read_regular` is now built; the crop picks its decoder from the file's bytes. About 60
  product and 150 test lines with the picture's own. Where the minutes went: the build with a stress probe on a second
  build server (about 21 million swaps of the captures folder against 2,000 writes, none outside); three review rounds
  (blockers 0, 0, 0), each with its own probes, and three fix rounds; four gates of 2 minutes; three hosted runs on the
  branch. Why past the hour: the rounds each found a test that did not pin its rule, and round 2 moved the regular-file
  open into platform so it has one owner.
- The window's save is written beneath the project root, 2026-11-08: builder launched 00:47, main 01:57 (79a2924e5,
  5fadddd2b, 3672c3d5c), 70 minutes, past the maintainer's hour. `file.write` checks its version through the read
  resolver and writes through `FilesMode::write`; the old writer with its fixed temporary name is deleted; a save at a
  name that is a link, or through a link at a folder of its path, is refused `bad_node_id` and writes nothing, the link at
  the name before any version check. About 65 product lines added and 78 removed, and 89 test lines. On Windows a save of
  a file another program holds open now fails where the old writer's rename replaced it (WIN-SAVE-HELD-OPEN). Where the
  minutes went: the build with a stress probe on a second build server (4,321 swaps of a folder of the path against
  1,000 saves, none outside); two review rounds (blockers 0, 0) with probes and mutants, and two fix rounds; three gates
  of 2 minutes; two hosted runs on the branch. Why past the hour: round 1 took 39 minutes (00:54 to 01:33: its probes,
  its mutants, and a disassembly of the Windows standard library's rename, which found the held-open difference), and
  round 2 found the contract paragraph wider than the code, which took a second fix round.
- On Windows a final name held open with delete sharing is replaced, 2026-11-08: builder launched 01:43, main 02:15
  (2658a328d, 4ec7bced6, 824416005; the first round's d4127decb moved onto main), 32 minutes. The write beneath a root
  renames by the temporary file's handle with the classic replace and, when that answers access denied, again with a
  POSIX-semantics replace, as `std::fs::rename` does; a name held without delete sharing still fails after the retry
  window, with the classic answer. A link at the final name, to a file or a folder, is now replaced and never followed
  on every system: the second call replaces a junction or a directory symlink there, as Unix's rename already did.
  Closes WIN-SAVE-HELD-OPEN. About 14 product and 46 test lines. Where the minutes went: the build (2 minutes); the
  hosted branch run, which showed the junction at the final name replaced (round 1's blocker); two review rounds
  (blockers 1, 0) and two fix rounds; five one-job Windows probes (each new case red on main, every beneath test green on
  the change, the directory symlink case red by name); two gates of 2 minutes.
- The window's new folder is created beneath the project root, 2026-11-08: builder launched 02:18, main 03:02
  (0ced502a3, 84485924a, aa01be3f2), 44 minutes. Platform's `create_dir_beneath` shares the write's one walk per system
  (the root and each parent opened relative to the one before, following no link) and creates the last name relative to
  its parent, so an entry of any kind at the name is `AlreadyExists` and is never followed; the parent is synced.
  `dir.create` calls it through `FilesMode::create_dir`: a link or a file at a folder of its path answers `bad_node_id`,
  and a link at the name itself now answers `already_exists` (it answered `bad_node_id` before); every other reply keeps
  its code, through one files-mode lookup that `file.read`, `file.write`, `file.delete` and `dir.create` share. The
  confined resolver now serves the delete alone. About 175 product lines added and 112 removed (the walk shared), and
  132 test lines. Where the minutes went: the build (7 minutes) with a stress probe on a second build server (621 swaps
  of a parent against 1,000 creates, none outside); two review rounds (blockers 0, 0), the first with 19,000 swap calls
  of its own, and three fix rounds (the last text only); three Windows probes and one hosted run on the branch; three
  gates of 2 minutes.
- A concept note is written beneath the project root, 2026-11-08: builder launched 03:05, main 03:31 (68d296149,
  3968b8969), 26 minutes. `ConceptStore::write` keeps its string rule and reply path and writes through platform's write
  beneath a root, which creates the `.concept` folders and takes a fresh temporary name; its own temporary-file writer
  and `create_dir_all` are deleted. A refused note answers `concept_write_failed`, as before. Platform's guarantee 17 no
  longer lists its callers; the integration table's row is the one list. About 12 product and 58 test lines. Where the
  minutes went: the build (3 minutes); one review round (blockers 0) with swap probes on local disk and the network
  home, and one fix round of text and test names (FE-CONCEPT-SAVE-SILENT and FILES-CONCEPT-NAME-RULES noted); one hosted
  run on the branch; two gates of 2 minutes, and one stray gate of the first commit run by mistake.
- The foreign-peer row test reads the close whichever comes first, 2026-11-08: red seen on main's hosted macOS run at
  03:43, builder launched 03:44, main 03:57 (d1dc8f8f3, 1bb28a4c0, 01c49d74a), 13 minutes from the builder. Main's
  hosted run for 3968b8969 failed `a_peer_of_another_uid_is_dropped_at_accept` on macOS: the server dropped the foreign
  peer before the test set its read timeout, and xnu refuses a socket option on a socket shut in both directions
  (`sosetoptlock`), so the option call answered EINVAL. The test now takes that answer on macOS as the close having come
  first and still reads the close; Linux keeps the loud failure. About 8 test lines. Where the minutes went: the build;
  two one-job macOS probes with a sleep that puts the drop first (red on the old test, green on the new); one review
  round (blockers 0), which found the kernel rule and a second record of macOS's refusal that disagrees
  (MACOS-SOCKOPT-RECORDS noted); two gates of 2 minutes.
- The REPL's figures are written beneath the project root, 2026-11-08: builder launched 03:35, main 04:25 (318531952 and
  48162048e moved onto main as eb1dec5d9 and a9f4edfa4), 50 minutes. `repl.execute` writes each figure through the
  workspace's `FilesMode::write` (folders created, a fresh temporary name, no link followed); a figure that cannot be
  written is logged and ends the spill, and the reply now carries the figures written before it (it carried none on any
  failure before); the reply's paths keep their form. About 46 product lines added and 23 removed, and 62 test lines.
  Where the minutes went: the build (14 minutes); one review round (blockers 0) with swap probes (main's spill wrote
  outside the root under them, the change never), and one fix round (the kept figures tested, a log assertion tied to its
  run, the records placed with their owners, REPL-FIGURE-SILENT noted); one hosted run on the branch; two gates of 2
  minutes, the second after the move onto main past the capsule test's landing.
- A record is written relative to its folder, opened once, 2026-11-08: builder launched 04:28, main 05:20 (2d00dba1c,
  e568e0521, 8f0315c28), 52 minutes. `write_durable` opens the record's folder once, by path, following a link, and
  only as a folder (a file, pipe or device there answers at once); the stale `<name>.tmp`'s removal, the exclusive
  create, the write and sync, the rename and the folder's sync all run relative to it, through the steps the write
  beneath a root already had, factored into one `write_in_folder` (the temporary name fresh for the write beneath a
  root, fixed for a record). On Windows the by-handle rename with its held-open fallback and the folder's flush replace
  the path rename, and the stale name is deleted with POSIX semantics. About 137 product lines added and 76 removed, and
  83 test lines. Where the minutes went: the build (4 minutes); two review rounds (blockers 1, 0: the folder opened
  without `O_DIRECTORY`, so a pipe at its path would hold the write) and two fix rounds; three hosted runs on the branch
  and four one-job Windows probes (the stale name held open red on the first commit; one probe showed a Windows check
  pinned nothing); three gates of 2 minutes. NOREPLACE-NFS-EINVAL noted.
- On Windows the relative open refuses a file at a record's folder by itself, 2026-11-08: decided 05:20, main 05:25
  (f06e68542), 5 minutes. The check P8's second round added pinned nothing (its test passed on Windows without it), so
  it, its import and its comment are deleted. About 6 product lines removed. One Windows probe, one gate of 2 minutes.
- The scratch guard test's probes write their files whole, 2026-11-08: red seen on main's hosted Windows run at 05:33,
  diagnosis launched 05:34, builder 05:52, main 06:08 (366a34551, ebc49bc19, 29ac38c46), 35 minutes from the red. The
  concurrent-runs case of `test-scratch-guard.ps1` polled a file its long probe was still appending line by line with
  `Add-Content`, which opens with write sharing only, so an append that met a poll read failed and the probe exited 1. Each
  probe now writes its file whole under another name and renames it into place, the daemon probe's later line is appended
  with sharing for readers, and the failure details print what each check tested and the run's stderr. About 30 test
  lines. Where the minutes went: the diagnosis (17 minutes, from PowerShell's own source); one-job Windows probes with a
  patch that holds the file open as a poll read does (red on the old test, green on the new, three times through the
  rounds); one review round (blockers 0) and two fix rounds, the second for an unquoted string the session caught that
  would have run as a command; two gates of 3 minutes. SCRATCH-GUARD-RMDIR-STOP noted.
- The rows' activation barrier is on the hold tool, 2026-11-08: builder launched 05:29, main 06:30 (a409f7a3a moved onto
  main as 4214e54e3), 61 minutes, a minute past the maintainer's hour. `SOT_TEST_ACTIVATION_BARRIER` and its markers give
  way to the points `rows.activation.start`, `rows.watchdog.restart`, `rows.activation.wait_for_settle` and
  `rows.activation.completed`; an ordering a test claims that no longer holds now fails the test where the barrier timed
  out after 30 seconds and went on. About 8 product lines added and 69 removed; the tests about 64 added and 117 removed.
  The tests take as long as before (84 and 11 seconds): no passing run waited on the old timeout. Where the minutes went:
  the build (26 minutes: the capsule workspace suite runs about 9 minutes here and ran whole, with a mutant per point);
  one review round (blockers 0), which ran the suites on old and new trees (30 minutes); one hosted run on the branch; two
  gates of 2 minutes, the second after the move onto main past the scratch guard's landing. Why past the hour: the suite's
  length, run whole by both builder and reviewer.
- The watchdog-first test arms no point it does not need, 2026-11-08: decided 06:33, main 06:39 (3c3e8f718), 6 minutes.
  The activation landing's review showed the test's `start` arm forced nothing (the guard and the backoff line order the
  test); the arm, its wait and its release are deleted, and one comment is re-wrapped. 3 lines added, 5 removed. One gate.
- The spawn suite's stub daemon answers the host query, 2026-11-08: builder launched 06:34, main 07:06 (af43bea4c moved
  onto main as 376cca8ff, then 23c7a4522), 32 minutes. Each spawn in `test-spawn-capsule-workspace.sh` asked the stub
  daemon for its host (`version.query`, the first step of `comm-spawn.sh`), which the stub never answered, so every spawn
  waited out the send window of 10 seconds and then took the local path. The stub now answers with the host the suite
  pins (`SOT_SELF_HOST`), and a new case in `test-spawn-remote-no-local-row.sh` pins that a daemon declaring no host stays
  local, the path the old wait had exercised by accident. The suite went from about 225 seconds to 29 on a second build
  server, all 17 cases kept; no other suite's stub misses the query. About 25 test lines. Where the minutes went: the
  trace (9 minutes, through a trace stream that survives the stub's own redirection); one review round (blockers 0) and
  one fix round; two gates of 2 minutes.
- A Claude row leg runs the selection hook on every prompt, 2026-11-08: builder launched 07:10, main 08:12 (db973e652,
  2b56871a3, bba94ae2f), 62 minutes, past the maintainer's hour. The selection plan's A5b, A5c and A5d as one piece:
  `agents/sot-selection-hook.sh` reads its whole input, then prints the file `SOT_SELECTION_FILE` names when it is a readable
  regular file, else nothing, and always exits 0; every Claude leg's `--settings` runs it at `UserPromptSubmit`, by a
  command that drains its input and does nothing when the script is missing; the supervisor's launch writes the row's
  `selection-hook` marker exactly when the leg carries the variable and the hook, so a row without it answers `held`; and a
  test drives the path from a connection standing in for the window, through the daemon's file, to the hook's output and
  its withdrawal. About 62 product lines and 330 test lines. Where the minutes went: the build (15 minutes); two review
  rounds (blockers 1, 0) and two fix rounds; two hosted runs on the branch; three gates of 3 minutes. Why past the hour: the
  first round found that the hook never read its input, so a prompt past the pipe's buffer would have drawn an error in
  every Claude session once installed, and the second found the same in the no-op for a missing script.
- A window's drawing becomes the row's selection, 2026-11-08: builder launched 08:26, main 09:10 (bcc93fb20, 7c4083bc6,
  7f4028e96), 44 minutes. The drawing plan's D3: `selection.set` with a drawing checks the window, the state root and the
  row, then under one process-wide compose lock reads the agent kind (`claude` only; a Codex row answers `no_agent` for a
  drawing and still takes text), has the files subsystem compose the picture on the blocking pool, renders the hook output
  that names it with its digest, and publishes it through the same path as text, withdrawn as text is. Text and drawing
  share one publish (`row_of` and `publish` split out of the text handler, its replies unchanged). About 139 product lines
  and 226 test lines. Where the minutes went: the build (9 minutes); one review round (blockers 0, four wording and test
  fixes: the file names the picture after each publish rather than always, the server contract names the new wait, and
  the ordering test, which had passed with the lock released early and taken about 30 seconds, became one held arrival
  of about a second through a hold point before the publish) and one fix round of 3 minutes; one hosted run on the
  branch; two gates of 3 minutes.
- The trust restart's two readers, 2026-11-08: builder launched 09:16, main 09:51 (9c21d4f41, f28a337be), 35 minutes. The
  first of three landings for REAUTH-TRUST-LOST's restart, built beside the second: `startup_of` tells Claude Code's
  folder-trust dialog from its main box and from every other dialog (the safety-check line, the trust option followed by
  the dialog's footer alone, and never the variant for a folder already trusted); `under_declared_prefix` asks the scope
  the trust record uses, through one helper both share; and each trust file keeps the instant of its last Claude
  preparation, for the restart's wait. Two real Linux captures of the dialog from Claude Code 2.1.319 are the fixtures,
  taken with a scratch home and configuration holding no credentials. About 117 product lines and 161 test lines; unused
  by product code until the restart lands. Where the minutes went: the build (7 minutes); one review round (blockers 0;
  the reader took the dialog's words quoted above another dialog for the dialog, which would restart a working session,
  and each of its four conditions lacked a test row) and one fix round of 4 minutes; two gates of 3 minutes.
- The trust restart's leg record, 2026-11-08: builder launched 09:16, main 09:59 (28893f299, 402d95e35), 43 minutes. The
  second landing: a row's watch state keeps the argv of the Claude supervisor spawned at its current generation whose agent
  has not shown its main box, and whether a trust restart ran since the last main box, so a row is restarted at most once
  per episode; the start path and the watchdog's restart open it, and the next spawn closes it. About 80 product lines and
  87 test lines; the two places that open the record are read, not tested, since a unit test cannot start a supervisor.
  Where the minutes went: the build (12 minutes); one review round (blockers 0; the generation rule of a main box
  unpinned, an unreachable branch, and logging under the row's lock) and one fix round of 3 minutes; two gates of 3
  minutes, the second after moving onto main past the first landing. Main's hosted run then failed on Windows in that
  landing's test: its failed spawn, on a state folder inside the root that did not exist yet, was not refused there.
  Fixed forward, 2026-11-08, red seen 10:13, main 10:23 (e4fa098af): the test creates the folder and asserts the refusal's
  own text; the product question went to `docs/defects.md` (STATE-ROOT-CHECK-NEW-FOLDER).
- The trust restart, 2026-11-08: builder launched 09:54, main 11:58 (108cc184d, 0f73aa32a, a6d3b975b), 124 minutes, past
  the maintainer's hour: a failure of the standard. The third landing: a Ready Claude row whose leg shows Claude Code's
  folder-trust dialog, with its root under the declared prefix, is stopped and spawned again with its own argv (a
  reauth's resume id kept), once per episode, under its guard and never waited on, never by keys; the spawn records trust
  again. The restart waits while this daemon started a Claude leg on the same trust file less than 2 s ago; a dialog it
  leaves still reads red. Found on the way: the dialog tick returned early when the comm registry file was missing, so a
  row with an open leg was never read; it now reads such rows. REAUTH-TRUST-LOST stays open: the tests use a stub agent,
  not the real lost update. About 264 product lines and 568 test lines. Where the minutes went: the build (38 minutes);
  two review rounds (blockers 0, 0; 33 and 32 minutes) and two fix rounds (7 and 3 minutes); three gates of 3 minutes and
  two hosted runs on the branch, beside the reviews. Why past the hour: the change itself was too large for one landing
  after the split in three (about 220 product lines against a planned 150: the restart and the tick that calls it would
  have been two landings), and each review of a path that can stop a working session took over half an hour; the first
  round found a rule nothing tested (a dialog the restart leaves must still read red), whose fix reordered the restart's
  checks and needed a second round.
- The destroy rules' pins, 2026-11-08: builder launched 12:08, main 12:34 (d8028607a, a88e42cdb), 26 minutes. Three rules
  the window's contract claimed for destroying a session got their tests: the agent pane is cleared before the view moves
  to the neighbor, a lost link ends only its own computer's destroy notes, and dropping a row keeps its computer's default
  view (the fixture's old second assertion could not fail). Tests and one test-only method; no product line. The review
  found that no landing runs the window's State fixtures, these two among them: the local gate and the per-landing hosted
  run compile them, and only the hand-dispatched release gate runs them (STATE-FIXTURES-RUN-ONLY-AT-RELEASE); the builder
  ran them under a virtual display on a build computer. Where the minutes went: the build (2 minutes); one review round
  (blockers 0) and one fix round of 1 minute; two gates of 2 minutes.
- The session start after a clear, its review's follow-ups, 2026-11-08: builder launched 12:08, main 12:56 (e3b3e28cf,
  e48b9cc21), 48 minutes, past the half hour. Both forms of the start print their shared lines from one place, so the full
  start's tests pin the again form; the again form reads no inbox, resolves its context without writing and so creates
  nothing in any comm folder, and names each failure in its own sentence (an unreadable registry gets no instruction to
  join, which could not help); a send and the again form share one classification of why a handle is not routable, with
  one table test over the four causes; the manual and the agents' contract no longer promise unread mail after a clear.
  About 70 product lines and 180 test lines. Where the minutes went: the build (2 minutes); two review rounds (blockers 0,
  0; the first found two causes untested, a copied decision, and wording wider than the tests) and one fix round of 2
  minutes; three gates of 3 to 4 minutes, the last after moving onto main past the destroy pins and their records. Why
  past the half hour: the second round, for the shared classification and the write-free context, which change product
  logic.
- The per-landing run executes the window's State fixtures, 2026-11-08: builder launched 12:40, main 13:46 (f387a2015,
  41dd99ba3), 66 minutes, past the maintainer's hour: a failure of the standard. The Linux rest-0 job installs a virtual
  display and Mesa's software Vulkan driver (with mirror retries) and, after the shard-coverage check, builds and runs the
  minimized target's 13 State fixtures alone; a hosted run names the device (llvmpipe) and each fixture, and a run with one
  fixture broken failed that job alone, naming it. It adds about a minute to that job, which does not set the run's wall
  clock. STATE-FIXTURES-RUN-ONLY-AT-RELEASE closed. About 30 workflow lines and record edits. Where the minutes went: the
  build (2 minutes); three hosted runs on the branch of about 20 minutes each, since the runner's adapter could be tested
  nowhere else (the first found the adapter but not the X11 keyboard library the window's event loop loads); one review
  round (blockers 1: the defect was not closed in the landing, which I had deferred to a records commit) and one fix round
  of 2 minutes; two gates of 4 minutes. Why past the hour: the premise lived only on the hosted runner, and each test of it
  was a 20-minute run; the library list should have been read from the event loop's sources before the first run.
- B2g, the window's network layer speaks chat, 2026-11-08: builder launched 13:49, main 14:27 (022576bfe, 92a9303af), 38
  minutes. fe-net carries the chat family as typed requests, replies and events: an old daemon's unknown chat operation
  becomes an unsupported-chat reply while every other request on the connection completes (M6); chat rides the window's
  existing link to a far host with no other login (M7, asserted in the one-login test); a post answered by a handler
  error, with no answer, with an unparsable reply or with an `other` outcome reads as unknown, is never replayed, and its
  pending entry is recorded before its write, so a write that ends the session still yields its unknown. About 168
  product lines with comments (over the planned 80 and the brief's 120) and 232 test lines. The pre-push guard refused the
  first push: a test fixture named a real person; the name was replaced before anything left the machine, and pushes are
  now swept against the guard's own list. Where the minutes went: the build (9 minutes); one review round (blockers 0) and
  one fix round of 1 minute; two gates of 3 minutes.
- A6, the window's network layer carries the selection, 2026-11-08: builder launched 14:30, main 14:51 (7bfbcb301,
  2eb1ab913), 21 minutes. fe-net carries `selection.set` as a typed request whose reply is one typed event carrying the
  request it answers; the request is never replayed, and one cut off by its connection's end yields an ended reply, so a
  selection that waited through a disconnect is never published after the person replaced or withdrew it (the drawing
  plan's orderings; the window's share re-sends the current state after a reconnect). About 70 product lines and 86 test
  lines. Where the minutes went: the build (1 minute); one review round (blockers 1: the brief had said replayable, from the
  older plan) and one fix round of 1 minute; two gates of 2 minutes.
- The session start after a clear, its second review's notes, 2026-11-08: builder launched 16:12, main 16:31 (bbb57f18e,
  9328fc87d), 19 minutes. A cleared session whose registry file is absent is told to join, which works from that state;
  one whose registry is present but unreadable (not JSON, no agents, not permitted, a directory or a dangling link at its
  path) gets no instruction, since a join cannot repair it; the contract and the hook header say so exactly. About 14
  product lines and 20 test lines. Where the minutes went: the build (1 minute); one review round (blockers 0) and one fix
  round of 1 minute; two gates of 4 minutes.
- B2h, the window's first chat owner, 2026-11-08: builder launched 14:31, main 20:07 (b14e92443, one commit squashed from
  five), 336 minutes, past the maintainer's hour: a failure of the standard. The owner keeps one watched channel matched to
  the daemon: each loss (a lost event bus, a status that says events were lost, an unreadable event, the link returning)
  is recovered by one path, a watch again until the daemon answers complete, then one read whose snapshot replaces the
  held records with the changes that arrived during it kept; watches are paced and capped before a gap, and a gap asks
  again once a minute. It holds only the top-level records the daemon tracks, at most the daemon model's newest 100,
  takes no reply to an abandoned request, posts one at a time, and reads a post of unknown outcome as unknown; fe-net no
  longer replays a queued chat watch or read after its connection ends. BUS-LOST-NO-RECOVERY now says only the chat
  recovers. About 262 product lines without comments (340 with them and blank lines) and 643 test lines. Where the minutes
  went: the build (8 minutes); four review rounds (blockers 3, 0, 1, 0; 19, 32, 23 and 22 minutes) and four fix rounds
  (8, 9, 9 and 3 minutes); the third round's first reviewer was refused by its API's safety filter at its first step,
  and its rerun on another model waited 174 minutes for the maintainer's word; seven gates of 2 to 4 minutes. Why past
  the hour: the change was about twice the size past which a change is split, and its rule is that the window's state
  match the daemon's after every order of replies and events, which each review built from the daemon's code and found
  broken in a new place; one of the first round's blockers was the brief's own rule (a status answered by a read, where
  the wire says a watch). The window's own second Slack read is the source of two of the classes found; a complete
  watch reply that carries the daemon's page would delete it (CHAT-TWO-READS-DISAGREE).
- A7a, the window shares its text selection, 2026-11-08: builder launched 20:12, main 21:03 (12c823e0f, one commit
  squashed from two), 51 minutes. The window's selection share is a per-frame reconcile (the drawing plan's share, which
  wins over the selection plan's list of gestures): the desired state is the selected row's released, non-empty flowed
  selection over a shown text preview, with the range, length and digest the daemon checks; it sends one
  `selection.set` when that differs from the state last sent and nothing is in flight, clears the row that held the
  share at a row switch, sends the latest change at the reply, shows a refusal once and does not retry it, forgets a
  host's share at its connection's end or a failed send and sends again after the connection returns, drops a reply
  from an ended connection, and keeps hosts apart; each reply kind has its status line. The plan's 150 product lines
  were split before launch; the manual's section and the done test through a window are A7b. About 124 product lines
  in the owner plus 24 of accessors and glue, and 214 test lines. The builder stopped once, as its brief said, on a
  needed one-line export in fe-net, which the session allowed. Where the minutes went: the build (9 minutes, with the
  stop); one review round (blockers 0; 23 minutes) and one fix round of 12 minutes; two gates of 2 minutes.
- D4a, the window's drawing, its pure owner, 2026-11-08: builder launched 21:07, main 22:30 (74eb2a339, one commit
  squashed from three), 83 minutes, past the maintainer's hour: a failure of the standard. The first of D4's three
  landings (the plan's 280 product lines split before launch): `InkLayer` holds a row's drawing, its cleared copy, the
  live stroke, drawing mode and a revision every committed change raises. A press inside the image rectangle maps to
  the base's fraction at any zoom and pan; drag points are clamped to the base and kept one raster pixel apart; a
  release, a new press or a lost focus commits, a base no longer shown drops the live stroke, and a press on a full
  drawing starts none. Undo and clear act only on a shown drawing; a right-button drag inside the image erases each
  stroke its path passes within the painted ink's reach of, as one undo step recorded from its first hit. Not yet
  wired. About 190 product lines and 460 test lines. Where the minutes went: the build (6 minutes); two review rounds
  (blockers 2, 0; 20 and 27 minutes) and two fix rounds (15 and 6 minutes); the move of this session to another account
  (2 minutes); three gates of 2 minutes. Why past the hour: the owner came out at 172 product lines against the brief's
  120 stop, and the builder neither counted nor stopped; the right-button erase, added to the plan from the
  maintainer's note, had no orderings with the keys pressed during a drag (the first round's panic) and a reach of one
  raster pixel, under one screen pixel at fit (the plan now says the painted ink's reach).
- D4b, the window's drawing, its raster and overlay, 2026-11-09: builder launched 22:33 (the 8th), main 00:05
  (63ced6673, one commit squashed from three), 92 minutes, past the maintainer's hour: a failure of the standard. The
  owner keeps the drawing's raster at the wire's dimensions for the base's source size and paints it with the wire's own
  functions, a segment per drag and a whole repaint per undo, clear, restore, erase hit, dropped stroke or new base, with
  a dirty rectangle inside the raster; the raster view records the node its quad came from, and one decider names the
  shown base, so the overlay is drawn only over its own figure, at its rectangle and scissor. The wire's contract now
  says only the daemon paints off the UI thread; the window's repaint runs on it (measured 36 to 243 ms at the point
  bound for hand-drawn strokes). About 143 product lines and 300 test lines. Where the minutes went: the build (7
  minutes); two review rounds (blockers 1, 0; 22 and 38 minutes) and two fix rounds (11 and 7 minutes); three gates of 2
  minutes. Why past the hour: the brief named the plan's decider of the shown base (3.4) only by section, and the build
  gated the overlay on the file last requested, which the first round found drawing one figure's ink over another;
  the first round's tests let 13 of 20 faults through, and the second round's longest was its 56-second test (now 6).
- D4c1, the window's drawing takes the mouse and its keys, 2026-11-09: builder launched 00:08, main 00:58 (f0c0f6768,
  one commit squashed from two), 50 minutes. With drawing on over the shown base, a left press inside the image
  rectangle focuses the preview, clears the agent pane's selection and begins a stroke with no text selection; motion
  extends it, release commits it, a right-button drag erases; every button passes the quit question and the stayed
  notice first and a release is never blocked, so a click that answers either draws nothing. `d` turns drawing on and
  off over an image; `u` or `Primary+z` undoes and `Delete` clears, live only while drawing is on over a shown base. A
  lost focus commits a live stroke, a base no longer shown drops it, and the same figure at another size rebuilds the
  raster at once with a live stroke kept. The routing lives in the drawing's owner, and every gate asks its one decider
  of the shown base. About 161 product lines (the brief's stop was 120, and the fix round's 155; neither builder
  stopped) and 150 test lines. Where the minutes went: the build (9 minutes); one review round (blockers 0; 25 minutes)
  and one fix round of 12 minutes; two gates of 2 minutes. Of D4's four builders, all ran past the line stop their
  brief stated; the split before launch is what bounded each landing.
- D4c2, a row's drawing stays with its row, 2026-11-09: builder launched 01:01, main 01:59 (32aabd5b3, one commit
  squashed from two), 58 minutes. The drawing (base, strokes, cleared copy, undo history, mode) is one value the owner
  holds and the row's parked view carries, so each row gets back its own drawing after a switch, a new row starts
  clean, a destroyed row's view drops it, and a restore raises the revision, rebuilds the raster and keeps nothing of
  the row left; two rows whose figures share a relative path keep two drawings. A window State fixture pins the three
  park and restore calls; it runs only under a display, so it was run on the hosted runner before the landing (passed).
  The keybindings page lists the three drawing keys with what each does today. The brief's other item, the source size
  for a figure served downsampled, was not built: its premise (the preview reply carries the scale) was false, and it
  is INK-RASTER-SERVED-SIZE. About 59 product lines and 180 test lines. Where the minutes went: the build (4 minutes);
  one review round (blockers 0; 25 minutes) and one fix round of 13 minutes, with one item the session's brief worded
  ambiguously (the catalog's text); two gates of 2 minutes and one hosted run on the branch of 11 minutes.
- D5, the canvas, 2026-11-09: builder launched 02:03, main 02:39 (2ecd72a51, one commit squashed from two), 36
  minutes. `Shift+D`, live while the preview pane is focused and not editing, shows or hides the row's canvas, and the
  flag rides the row's parked drawing; under the refused-hello overlay it does nothing. While shown, the pane draws a
  white rectangle letterboxed to the wire's canvas size with the ink over it, drawing is always on, zoom and pan do not
  apply, and the page, image and edit keys and the wheel do not reach the hidden preview, which stays installed and
  untouched; another node's install hides the canvas, the same node's re-install does not. The pane's one decider gives
  the canvas its place (over the image and text, under the overlay and edit mode), and the owner's decider names it as
  the shown base, so drawing on it replaces a figure's drawing. About 121 product lines added and 45 removed, and 140
  test lines. Where the minutes went: the build (6 minutes); one review round (blockers 0; 19 minutes) and one fix round
  of 5 minutes; two gates of 2 minutes.
- D6a, a daemon says it takes a drawing, 2026-11-09: builder launched 02:42, main 03:11 (6ca7e3efc, one commit squashed
  from two), 29 minutes. A daemon built before the selection op's drawing field reads a drawing as a clear and answers
  shared, and the wire keeps policy off the version string (its guarantee 5), so the plan's gate on the daemon's
  version became a declaration: the hello reply gains a defaulted `takes_drawing`, written only when true, which this
  daemon sets and the last release's reply reads as false (the protocol integer is unchanged). Each fe-net connection
  keeps its own hello's answer, and a drawing on a connection whose hello did not take one is answered unsupported and
  never written; a clear and a text selection still go. The review's fix moved the check from a per-host copy, which a
  key handler could read stale before the next frame, to the send on the connection itself. About 13 product lines and
  80 test lines across the wire, the daemon's hello and fe-net. Where the minutes went: the build (4 minutes); one review
  round (blockers 0; 16 minutes) and one fix round of 4 minutes; two gates of 2 minutes.
- D6b, the window shares its drawing, 2026-11-09: builder launched 03:15, main 03:45 (f93141cfe, one commit squashed
  from two), 30 minutes. The selection share's desired state gains the selected row's drawing while its base is shown
  and it has a committed stroke, identified by the drawing's revision and read from the drawing's owner; it is sent as
  the request's drawing, a drawing beats a text selection, and a clear never carries an empty drawing. The share holds
  while a stroke or an erase drag is live, so one drag sends once, at its end; the rest of the share's orderings hold
  for the drawing as for text. Each reply kind has its status line, a daemon that cannot take a drawing included. The
  manual's drawing section is written, the requirement's status says it is built for Claude rows and proved by unit
  tests, and SCRATCH-PAD is closed. About 54 product lines added and 11 removed, and 170 test lines. Where the minutes
  went: the build (4 minutes); one review round (blockers 0; 17 minutes) and one fix round of 4 minutes; two gates of 2
  minutes. With it, the drawing plan's pieces through D6 are on main, in nine landings since D3; D7 (Codex) waits on
  its premise.
- A7b, the manual says how a selection or a drawing reaches the agent, 2026-11-09: builder launched 03:49, main 04:08
  (5f040b7ae, records only, one commit squashed from four), 19 minutes. One section of the previews guide, "Sharing with the
  agent", says what a session's agent is shown, what it is told and when, what withdraws a share, and each status line
  with its causes; the two requirements' statuses say what is built. Two notes found writing it:
  SELECTION-CODEX-HELD and SELECTION-WITHDRAWN-ON-REFRESH. Where the minutes went: the first draft (1 minute); two
  review rounds (blockers 1, 0; 5 and 7 minutes: the first found a false claim about child sessions and eight wider
  than the code) and two rewrites of 1 minute each, then two sentences the session corrected against the plan's
  drawing rule; one gate of 2 minutes. The first reviewer checked out the branch in the shared checkout for a moment,
  which its brief had not forbidden; the briefs since say it.
- A Codex row answers a selection no_agent, 2026-11-09: builder launched 04:12, main 04:34 (f2b056852, one commit
  squashed from three), 22 minutes. The daemon took a selection for a Codex row, but only Claude legs carry the prompt
  hook and its marker, so a Codex row answered held and the window advised a restart that could not help. Until the
  Codex wiring is built, only Claude rows take a selection, as only they take a drawing; the agents contract states the
  wiring's untested premises once, a test pins that a Codex leg writes no marker, and SELECTION-CODEX-HELD is closed.
  One product line and about 15 test lines. Where the minutes went: the build (2 minutes); two review rounds (blockers
  1, 0; 7 and 5 minutes: the first found the contract still saying a Codex row answers held) and two fix rounds of a
  minute each; two gates of 2 minutes.
- A thinner pen, 2026-11-09: builder launched 10:29, main 10:47 (9359b3ef2), 18 minutes. The maintainer, drawing on
  the first candidate with selection and drawing: "could we make the pen a bit smaller?" The ink's width is 1/300 of
  the raster's long side instead of 1/200, the 3-pixel floor kept; the window's raster, the daemon's picture and the
  eraser's reach share the one rule. One product line and about ten test lines. Where the minutes went: the build (4
  minutes, the red at the base shown); one gate of 4 minutes, scoped; one review round of 13 minutes (blockers 0; its
  eight mutants of the rule were all killed); its wording findings went into the records after, and its two test
  findings into INK-REACH-ONE-SIZE.
- One definition of a whole-line range (SELECT-MARKDOWN, landing 1 of 2), 2026-11-09: builder launched 11:12, main 11:43
  (2a564496f), 32 minutes: past the half hour, a failure by this page's standard. `sot_protocol::ops::whole_lines`
  builds the range and text of whole file lines as a selection names a file's text, and a backend test proves the
  daemon's unchanged check accepts exactly that over every line pair of a fixture in LF, CRLF and without its final
  newline; no product code calls it yet. About 30 product lines and 90 test lines. Where the minutes went: the build (4
  minutes); two review rounds (blockers 0, 0; 12 and 10 minutes) with one fix round of 3 minutes between; two gates of 2
  minutes, scoped. The cause of the overrun: the plan wrote this landing's records as if the next landing's caller
  existed, and its backend test asserted only that one pair was accepted, so a version answering nothing for most pairs
  passed; the first review found both, and the second round was the cost. A plan's landing states its records true at
  that landing alone, and a test over many cases asserts the exact count. The second round's one finding, a test of a
  line ending in a carriage return at the end of the text, goes into landing 2. The amended commit's push over its own
  branch was refused by the session's command check as destructive and not retried; it went up under a new branch name.
- A rendered markdown selection is shared as the file's lines (SELECT-MARKDOWN, landing 2 of 2), 2026-11-09: the walk
  marks each paragraph's, heading's and code block's spans with their file lines (a front matter's lines added), a new
  `SourceLines` maps each rendered buffer line's bytes to them, and `released_selection` shares the whole file lines
  the selected text was rendered from, through `whole_lines`; a rendered buffer with no map shares nothing, and the
  walk's golden is unchanged. SELECT-MARKDOWN is closed before its window test; SELECTION-WITHDRAWN-ON-REFRESH names
  the reflow that withdraws a rendered selection. About 110 product lines and 190 test lines. Builder launched 11:45,
  main 13:10 (d7456b9af), 85 minutes: far past the half hour, a failure by this page's standard. Where the minutes
  went: the first builder (8 minutes) was stopped when the session's account reached its five-hour limit and the
  session moved to another account; a second builder checked and kept the first one's work whole and committed it (4
  minutes); one gate of 2 minutes, scoped; one review round of 70 minutes (blockers 0; an audit of 41 documents not in
  the tests at two widths in both line endings, all lined up, and ten mutants), of which about 30 went to one long
  command. The causes: a review brief that asked for an open-ended audit and mutant builds with no time bound, and the
  account move. A review brief names a bound for its audit and mutants, as the probes-first note says. The review's
  record findings went into the records after; its test findings into SELECT-MARKDOWN-PINS.
- The containment test ends the pipe holder it starts, 2026-11-09: diagnosis launched 13:15, main 13:41 (e8e9a7764), 26
  minutes. The whole local gate on main failed with every suite passing and one `sleep 30` left behind; a containment
  test's escaped pipe holder was never ended and was caught only when that short job ran within 30 seconds of the
  gate's end. The holder now holds the pipe only while the test's keep file exists, and the test sees it gone. About 20
  test lines. Where the minutes went: the diagnosis (17 minutes, the leftover's process id placed among the jobs'); the
  build (2 minutes, the leftover shown at the base and gone after); one whole gate of 3 minutes, unscoped and clean;
  one review round of 6 minutes, bounded (blockers 0). The class, a census that runs once at the end, is
  GATE-LEFTOVER-PER-JOB.
- The markdown selection's fixture stays LF on every checkout, 2026-11-09: builder launched 13:39, main 13:47
  (18398727c), 8 minutes, a fix forward of the hosted Windows run that the rendered markdown selection's landing turned
  red. A Windows checkout gave the test fixture CRLF, and the tests' own CRLF form of it then held `\r\r\n`, which comrak
  counts as two line ends; `.gitattributes` now keeps that folder's markdown LF. Four lines. Where the minutes went:
  the build (3 minutes, the four failures shown in a clone that checks out CRLF as Windows does, and gone after); one
  whole gate of 3 minutes; one review round of 4 minutes, bounded (blockers 0). A Windows checkout made before this
  commit keeps its CRLF copy until the fixture is checked out again (SELECT-MARKDOWN-FIXTURE-CRLF).
- The project chat's step R0 (records only), 2026-11-09: builder launched 14:06, main 15:13 (7a10fb5b7), 67 minutes,
  of which about 45 the session held it ready off main while a release question named main's commit, a hold the
  maintainer asked about and that is not repeated. The manual's configuration page gains the chat's two files as the
  code reads them today, the defect list three notes, and the requirements the project chat's paragraph the maintainer
  approved. Where the minutes went: the build (6 minutes); two review rounds (blockers 1, 0; 10 and 6 minutes: the
  first found the manual describing mail delivery and a chat drawer that do not exist) and three fix rounds of 1 to 2
  minutes; the hold.
- The status line warns when the home folder nears its quota, 2026-11-09: builder launched 15:14, main 15:33
  (9d4aa8b4a), 19 minutes, at the admiral's request after per-user limits on the shared home were set. A `home N%`
  item on the Claude Code status line's second line at 90% or more of the user's limit, red at 95% or more, read
  from the file server's usage file for the running uid, and nothing when the file, the row or the limit is missing
  or malformed. About 12 product lines and 66 test lines. Where the minutes went: the build (2 minutes); three scoped
  gates of 2 to 3 minutes (the first failed the suites' rule that every shell suite loads the home guard first); one
  review round of 11 minutes (blockers 0; a decoy uid that prefixes the real one, missing from the test, was added);
  two fix rounds of a minute.
- The project chat's B5a, the Chat tenant, 2026-11-09: builder launched 15:13, main 16:01 (41b9ae031), 48 minutes, past
  the half hour, a failure by this page's standard. The drawer has a fourth tenant, Chat (`Primary+Shift+k`), which
  shows nothing yet, and one exhaustive decision of who a tenant's input reaches, so the Monitor no longer types into
  or scrolls the hidden REPL (MONITOR-TENANT-REACHES-REPL closed). About 95 product lines and 50 test lines. Where the
  minutes went: the build (6 minutes); two review rounds (blockers 1, 0; 16 and 15 minutes: the first found the
  guarantee unpinned, a deleted check passing every test); one rework of 3 minutes; two scoped gates of 2 minutes. The
  cause of the overrun: the plan's red test covered the maps but not the handlers that read them, so the first review
  found the guarantee untested and a rework and second round followed. The handlers' arms are still driven by no test
  (B5A-ARMS-UNDRIVEN).
- The project chat's B5b, the owner moves to a leaf module, 2026-11-09: builder launched 16:02, main 16:14 (5673fc010),
  12 minutes. `ChatOwner` is in `drawer/chat/owner.rs` with its tests, and the window's `impl State` stays in `mod.rs`,
  so the owner's fields are reachable only through its methods (a read from `mod.rs` no longer compiles). A move: one
  visibility line. Where the minutes went: the build (3 minutes); one scoped gate of 2 minutes; one review round of 8
  minutes, bounded (blockers 0; 310 of 340 lines byte-identical).
- The project chat's B5c, the watch follows the shown row, 2026-11-09: builder launched 16:16, main 17:10 (165ec8b56),
  54 minutes, past the half hour, a failure by this page's standard. The window watches exactly the channel of the row
  the drawer shows, while it shows Chat or Help opened from Chat, and nothing otherwise; a resolve answer other than
  `mapped` ends the watch. About 150 product lines and 140 test lines. Where the minutes went: the build (6 minutes),
  which ran past the plan's named split point at 120 product lines to about 210, then a split into this landing and
  the reads-in-flight set (3 minutes); two review rounds (blockers 2, 0; 19 and 16 minutes: the first found a stale
  channel kept watched after a link change and when the watch runs untested, three mutants passing the suite); one fix
  round of 7 minutes; two scoped gates of 2 minutes. The causes: a builder that did not stop at the split, and a red
  test that covered the row channel's decisions but not the conditions that start and stop it.
- The project chat's B5c2, the reads in flight, 2026-11-09: split from B5c's build 16:25, rebased onto main as its own
  landing 17:12, main 18:08 (536cf56fe), 56 minutes from the rebase and 103 from the split, past the half hour, a
  failure by this page's standard. The drawer keeps, per host, the set of history reads in flight; a read equal to one
  in flight waits, a read its reader gave up is an orphan whose reply is dropped, a held read given up is cancelled, each
  owner call gives up what it dropped in one place, and a link change clears the host's set, so a read from before a
  re-watch never fills the new watch. About 85 product lines and 140 test lines. Where the minutes went: 45 minutes
  behind B5c's review rounds before its own began; a rebase of 2 minutes; three review rounds (blockers 2, 1, 0; 18, 14
  and 14 minutes); two fix rounds of 3 and 2 minutes; three scoped gates of 2 minutes. The cause: the plan's red test
  pinned only the equal-read wait, and each review found another rule of the set with no test (the link clear and the
  held cancel; then the half of the comparison that keeps a read still wanted; then the two-host parts, left to B5c3).
- The project chat's B5d, a gap only after the daemon's slowest snapshot, 2026-11-09: builder launched 17:15, main
  18:13 (d39167f49), 58 minutes, past the half hour, a failure by this page's standard. The drawer shows a channel as
  not up to date only 20 s after the daemon admits its watch (any answer but `unavailable`), or after the recovery's
  start when no answer comes; a refused watch is a gap at once; the gap carries its reason, a refused read's code
  included; `stale_row`, `stale_mapping` and `unmapped` are not asked again (CHAT-GAP-ASKS-DEAD-TARGET closed). About
  57 product lines and 190 test lines. Where the minutes went: the build (4 minutes); three review rounds (blockers 1,
  0, 0; 12, 13 and 12 minutes); three fix rounds of 2 to 3 minutes and a records fix of 1; two rebases onto B5c2's
  rounds (1 minute each); four scoped gates of 2 minutes. The causes: the build removed the old attempt cap and the
  red test's cases all had reads that succeed, so a complete watch whose reads were all refused never became a gap
  (the first review's blocker), and a watch never answered was found in the second; its rounds also waited on
  B5c2's, since it was built on that commit.
- The project chat's B5c3, a link change clears and cancels only that host's reads, 2026-11-09: brief sent 18:08, main
  18:20 (518f7b0b0), 12 minutes. Two two-host tests pin the parts of B5c2's link-change rule its third review found
  untested (another host's reads stay; the host's held read is cancelled), with doc comments and the contract bullet;
  no product logic. About 70 test lines. Where the minutes went: the build (3 minutes); one review round of 8 minutes
  (blockers 0; the per-host match noted as CHAT-FLIGHT-HOST-UNPINNED); a rebase onto B5d (1 minute); one scoped gate of
  2 minutes.
- The project chat's B5e, the Slack text grammar in wire, 2026-11-09: builder launched 18:17, main 18:43 (a8a1edc62), 26
  minutes. One parser reads a record's text (`text_segments`: a bare `<` opens a token to the next `>` or the end, then
  exactly three entities are decoded, `&amp;` last), and one escape and one bound make a post's (`escape_text`,
  `post_text_fits`, 8 KiB escaped). About 38 product lines and 42 test lines. Where the minutes went: the build (4
  minutes, 63 product lines); two review rounds (blockers 1, 0; 10 and 8 minutes); two fix rounds of 2 and 1 minutes,
  the second in a worktree of its own after a brief of mine first pointed it at the worktree another builder held; three
  scoped gates of 2 minutes. The first review's blocker was a record: the plan's premise that no release carries
  `chat.post` was false (both 0.7.1 candidates do, at protocol 4, and send the text as given), so the version exception
  was deleted and the hub's hello says instead whether its door escapes (built in B5l).
- The project chat's B5l, chat's one door escapes every message text, 2026-11-09: builder launched 18:35, main 18:57
  (0e2b0ffd3), 22 minutes. Every message text the chat crate sends is made Slack's by one function (bounded, then
  escaped; empty or over the bound is `bad_request`, unsent), and a post's body carries exactly its keys, so no post
  can mention or ping; `HelloRes.posts_plain_text`, defaulted and left out when false, says the hub's door escapes.
  About 22 product lines and 96 test lines. Where the minutes went: the build (4 minutes); one review round of 12
  minutes (blockers 0; two false records and the body's keys untested); one fix round of 2 minutes; a rebase onto B5e's
  last commit (2 minutes); two scoped gates of 3 minutes.
- The project chat's B5f, the literal projection, 2026-11-09: builder launched 18:30, main 19:04 (450fcfb51), 34
  minutes, past the half hour, a failure by this page's standard. A record becomes kinded spans: a header (the name
  cleaned of controls and wire's unsafe characters, the origin tag, the local time by checked integer math, raw past
  range), the body through wire's grammar with markup its own kind and controls made U+FFFD, and meta lines. About 107
  product lines and 120 test lines. Where the minutes went: the build (3 minutes); two review rounds (blockers 1, 0; 12
  and 10 minutes: the first found a wire-legal stamp near chrono's limit that panicked the window east of UTC, and five
  of six mutants alive); two fix rounds of 3 and 2 minutes; three rebases onto B5e's and B5l's commits (1 minute each);
  four scoped gates of 2 minutes. The causes: a red test without the time format's edges or the meta rules, and the
  rebases while B5e and B5l moved under it.
- The project chat's B5g, the drawer shows the channel, 2026-11-09: builder launched 19:05, main 19:42 (de3089241), 37
  minutes, past the half hour, a failure by this page's standard. With Chat shown, the held records are placed oldest
  to newest with the newest at the body's bottom; only records that reach the body are measured and shaped, their
  buffers cached by spans, width and scale (those in view plus 64 others, the farthest evicted first), prepared as
  clipped areas in the frame's one text vector; `no messages` only for a channel read whole and empty. A new manual
  page, `guide/chat.md`. About 220 product lines with comments and 160 test lines. Where the minutes went: the build (5
  minutes); three review rounds (blockers 2, 0, then a check of my own; 12 and 8 minutes: the first found all six
  mutants alive, the order resting on an untested sort and a cache test that could not fail); two fix rounds of 6 and
  2 minutes; three scoped gates of 2 minutes. The causes: a red test of three placements that pinned none of the
  view's other rules, and a fix round whose mutant runs stacked when a restore failed, so a reviewer reran them one at
  a time.
- The project chat's B5h, every outcome has a state, a next action and words, 2026-11-09: builder launched 19:43, main
  20:27 (c0eff1431), 44 minutes, past the half hour, a failure by this page's standard. The Chat drawer's title says
  each outcome of the shown row's channel (unlisted, offline, reading, unmapped, no configuration, not the hub, the row
  gone, a refusal, no chat, a gap and its reason, a watch refused for good) and when it is asked again; a refusal
  code's words have one owner, wire's `ChatCode::text()`, which the daemon's refusal and the window both use. About 100
  product lines and 175 test lines. Where the minutes went: the build (6 minutes, with the last table row unbuilt);
  two review rounds (blockers 1, 0; 13 and 9 minutes: the first found that row unbuilt and its test pinning the wrong
  words, codes shown in Rust's spelling and six rules unpinned); two fix rounds of 8 and 2 minutes, the first moving the
  daemon's per-code text into wire; three scoped gates of 3 minutes. The causes: a builder that left a table row
  unbuilt and wrote its test to the words it had, and a fix that crossed three crates.
- The project chat's B5i, a reader scrolled up stays put, 2026-11-09: builder launched 20:28, main 20:59 (f7bde24c4), 31
  minutes, just past the half hour, a failure by this page's standard. The view's position is following or a record's
  top and offset; PageUp and PageDown move a third of the body, Alt+Up, Alt+Down and the wheel one line, only in the
  Chat tenant with its focus, and End returns to following; an anchor that goes passes to the nearest newer record; a
  row change returns to following. About 124 product lines and 160 test lines. Where the minutes went: the build (11
  minutes, past its 100-line stop, which it reported with a split point); one review round of 12 minutes (blockers 0;
  the focus rule, the clamp and the row reset untested); one fix round of 4 minutes; two scoped gates of 2 minutes. The
  cause: the build's size, about twice the plan's 60 lines.
- The project chat's B5j, tombstones, 2026-11-09: builder launched 20:32, main 21:31 (3fc1e22ab), 59 minutes, past the
  half hour, a failure by this page's standard. A message deleted while the window watches stays in place as
  `(message deleted)`: the owner holds each record with its deleted flag as one pair, keeping only the deleted record's
  identity, author, time and origin; a tombstone counts against the bound and a replacing snapshot drops it. About 32
  product lines and 120 test lines. Where the minutes went: a git slip in its first worktree (a stash command whose
  flag was read as a path, then a pop of an old unrelated stash) and a restart in a new worktree (5 minutes); the build
  (8 minutes); three review rounds (blockers 1, 2, 0; 12, 9 and 6 minutes: a tombstone marked by two wire flags the
  wire accepts, then a set beside the records whose drawer wiring was untested); three fix rounds of 9, 4 and 2
  minutes, the first waiting for B5i to land; four scoped gates of 2 minutes. The causes: a representation the plan
  left open and the builder chose from wire fields, and the slip.
- The project chat's B5k, the draft, 2026-11-09, in four landings: builder launched 21:11; the move of
  `chord_types_text` into `input/` (2e9ea678b) and the draft's own state (463b0e9f0) main 22:30, 79 minutes; the
  wiring (7efcfd230) and the rows (c46aa3690) main 22:39, 88 minutes; past the half hour, a failure by this page's
  standard. One draft per canonical row, holding only what wire's `post_text_fits` admits (a key or paste past it
  refused whole), its destination fixed at the first character, CR LF and CR made LF and other controls dropped as the
  drawer cannot show them, edited only in the Chat tenant (`DrawerInput::Chat`), drawn at the drawer's foot in at most
  four wrapped rows with a true count of rows not shown at every height; a retired row drops its draft. About 210
  product lines and 250 test lines over the four. Where the minutes went: the build (9 minutes, to about 207 product
  lines past its 110-line stop, the test written after the code); two splits, into three and then four commits (13
  minutes); a rebase onto B5j (3 minutes); four reviews in parallel (blockers 0, 0, 0, 1; 9 to 13 minutes, one by
  reading only because the build computer's root disk was full of the evening's review builds); one fix round across the stack (24
  minutes); a review of the whole stack (blockers 0; 8 minutes); a last fix of two commits (5 minutes); ten scoped
  gates of 2 minutes. The causes: a builder that did not stop at its size bound, so the change was split after it was
  built, and the full disk.
- The project chat's B5m, the post, 2026-11-09, in three landings: builder launched 22:43; the flag (760a6c2c9: a
  connection keeps whether its hello said the daemon posts plain text, and a post on one that did not is refused
  locally) and the name (9250e00ce: `[chat] name` in settings.toml) main 23:35, 52 minutes; the post (fa9070892) main
  23:40, 57 minutes; past the half hour, a failure by this page's standard. Enter posts the draft once, frozen while it
  waits, and only a `posted` reply for that post clears its own row's draft; every outcome is reported once, its words
  on the status line. About 180 product lines and 250 test lines over the three. Where the minutes went: the build (10
  minutes, which stopped at its size bound uncommitted, as its brief said); the split into three (5 minutes); three
  reviews in parallel (blockers 0, 1, 0; 9 to 10 minutes: part 2's manual said Enter posts before it did); one fix
  round across the stack (14 minutes); a review of the whole stack (blockers 0; 5 minutes; three of part 3's rules
  alive, two of them double-post risks); a last fix of part 3 (3 minutes); seven scoped gates of 2 minutes. The cause:
  a step of about 145 product lines planned as 55, so it was split after it was built.
- The project chat's B5n, older pages in the owner's one store, 2026-11-10, in two commits landed together: builder
  launched 2026-11-09 23:45, main 00:51 (a2f29504f, 3ec55215d), 66 minutes, past the half hour, a failure by this
  page's standard. The owner keeps up to 256 records above its floor, a hard ceiling of 276 while a reader stays on the
  oldest, trimmed in one pass after every change; a replacing snapshot replaces all, or keeps the older records when it
  overlaps; when the oldest held record is in view the drawer reads 20 older ones in a slot of its own, taken only while
  the floor it was asked below still holds and only as many as fit; a refusal, Slack's plan hiding history and an empty
  page each stop paging with words (CHAT-EMPTY-PAGE-NO-CURSOR noted). About 175 product lines and 365 test lines. Split
  into the store and the paging before it was built. Where the minutes went: the build (13 minutes); three review
  rounds (blockers 1 and 1 in parallel, then 0; 13 and 9 minutes: the store unbounded under a parked reader with a
  quadratic trim, and a page taken after its floor moved); three fix rounds of 13, 8 and 3 minutes; six scoped gates
  of 2 minutes. The causes: the plan's bound left open how a parked reader and the bound meet, and the paging rules
  were checked when a page was asked for but not when it arrived.
- The project chat's B5o, the message mark, 2026-11-10: builder launched 00:58, main 01:42 (07965073d), 44 minutes,
  past the half hour, a failure by this page's standard. ArrowUp and ArrowDown in the Chat drawer mark one held message,
  folded one key at a time; a mark moved out of view moves the view to show it; a mark whose header leaves the body
  moves to the nearest message whose header is inside, or to the first one shown when none is; a mark no longer held
  passes to the nearest newer one; a row change clears it; the marked header is drawn in the focus color
  (CHAT-MARK-SCROLL-SAME-FRAME, CHAT-MARK-ZERO-BODY and CHAT-RERESOLVE-KEEPS-ANCHOR noted). About 89 product lines and
  225 test lines. Where the minutes went: the build (13 minutes); two review rounds (blockers 1 and 1; 11 and 9
  minutes: the mark could sit on a sliver with its header off screen, then the builder's own fallback could still mark
  a one-pixel header); two fix rounds of 5 and 2 minutes, the second read by the session in place of a third review;
  three scoped gates of 1 to 2 minutes. The cause: the brief left the builder to define "in view", the one term the guarantee turns on, so each
  review found a definition that let the mark leave the person's sight.
- The project chat's B5p, one thread view, 2026-11-10, in three commits landed in two pushes: builder launched 01:46;
  the view (85f14caf7) main 03:15, 89 minutes; the owner's reserve and the thread's reads (6afab7bcf, 90002ae5f) main
  03:27, 101 minutes; both past the half hour, a failure by this page's standard. ArrowRight opens the marked
  message's thread and Escape, a row switch, hiding the drawer or its channel's watch changing closes it, the
  channel's place kept; the title keeps the link's words over a thread; the thread is read whole, 20 at a time through
  the reads in flight, each reply taken only for the read it answers, repeats dropped, up to 156 messages with the
  channel's older records giving way and never its newest 100; refused reads asked again on the pace with words
  (CHAT-THREAD-REREAD-CLEARS and CHAT-THREAD-RETRY-WHILE-DOWN noted). Posting into a thread is the next landing. About
  230 product lines and 535 test lines over the three. Where the minutes went: the first build, stopped at its size
  bound at about 200 lines for what the plan put at 55 (6 minutes); the split into the view and the reads (21
  minutes); the view's review (blockers 1, 8 minutes: the title hid a lost link) while the reads were split again into
  the reserve and the reads (13 minutes); the view's fixes and the stack rebuilt on them (17 minutes); a review of the
  stack (blockers 0, six should-fix, 14 minutes: a thread outliving its channel's watch, repeats within a page, a held
  read surviving a link change); a fix round (12 minutes); a second review of the reads (blockers 0, four should-fix,
  12 minutes); a last fix of tests and records (2 minutes, read by the session); seven scoped gates of 1 to 2 minutes.
  The causes: the plan sized the whole step at 100 product lines, half what its first part took, so it was split twice
  after it was built; and the reviews kept finding rules a thread shares with the channel (the title, the watch, the
  reads in flight) that the plan stated for the channel alone.
- The project chat's B5p, its last part, posting into a thread, 2026-11-10: builder launched 03:31, main 04:08
  (ae5a44dcb), 37 minutes, past the half hour, a failure by this page's standard. A draft begun while a thread is open
  posts into that thread and keeps that destination through a close, a row switch and a reconnect; a channel draft
  stays the channel's; the line under the draft names where it goes; after the person's own post into the open thread
  it is read again whole through the one restart, which gives up the read in flight first; the thread is matched by
  host, workspace, channel and parent, whatever row posted. About 75 product lines and 150 test lines. Where the
  minutes went: the build (5 minutes); two review rounds (blockers 1, then 0; 10 and 9 minutes: a post made while the
  thread's first page was in flight took that page's old answer as its reread, so the person's reply did not show and
  could be posted twice); two fix rounds of 6 and 3 minutes, the second read by the session; three scoped gates of 1 to
  2 minutes. The cause: the brief named the reread after a post but not its race with the read already in flight,
  the value matching the plan states for every chat read.
- The project chat's B5q, selection and copy, 2026-11-10, in two commits landed together: builder launched 04:11, main
  04:50 (4a2be095b, bbfdab4fc), 39 minutes, past the half hour, a failure by this page's standard. A left press in the
  chat's records focuses the drawer and a drag selects text inside one message's body, the preview's selection over that
  record's buffer, cleared by any press elsewhere, an edit, a new width or scale, the record dropped or evicted, the
  mark moving onto or off it, a row reset and a thread opening; the tint joins the one batched selection paint;
  Primary+c copies the selection or the marked message's shown text, header excluded; a chat selection is never shared
  with the agent. About 125 product lines and 355 test lines over the two (test modules inside product files
  counted as test). Where the minutes went: the build, stopped
  at its size bound at about 122 product lines with no test yet (3 minutes); the split into the selection and the copy,
  with every test written then (13 minutes); one review of the stack (blockers 1, 14 minutes: opening a thread set the
  channel's view aside with its selection, so a press during the thread could not clear it and it came back at close);
  one fix round (5 minutes, read by the session); four scoped gates of 2 minutes. The causes: a step planned at 65
  product lines came to 122, and the thread view's parking of the channel's view, landed one step before, carried
  state the plan had not yet given it.
- The project chat's B5r, the native witness, 2026-11-10: builder launched 04:52, main 05:19 (2405fbbf0), 27
  minutes, inside the half hour. A fifteenth State fixture drives the real window's chat through its State: a row
  listed and Chat shown, a mapped resolve, a watch, a snapshot and older pages to 200 messages, PageUp to the oldest and
  back, the wheel through the window's own wheel code, Alt+Up, a resize of the canvas with 256 records of 8 KiB, the
  mark, a thread opened, read and closed, a typed and pasted draft posted once, the link dropped and back and a lost
  feed; it prints the frame times and asserts none (`chat-witness frames=431 max_ms=283.6 p95_ms=9.4
  resize_ms=195.8`, under a virtual display on another computer, since the build computer lacks a library the window
  needs). Its limits: the window's own resize event and the two key handlers are not run (CHAT-WITNESS-KEYS-COPIED
  noted). No product lines; about 360 test lines. Where the minutes went: the build (12 minutes); one review (blockers
  1, 9 minutes: the wheel step skipped the window's wheel code that another fixture already calls); one fix round (3
  minutes, read by the session); two scoped gates of 2 minutes. Hosted runs on main tonight: two Windows reds in
  other subsystems' tests, each passing on the runs around it (BENEATH-TWO-WRITERS-WINDOWS and
  REGISTRY-TEMPFAIL-LOCK-TURN noted).
- Platform's write beneath a root on Windows closes its temporary file's handle at the rename, 2026-11-10: builder
  launched 05:33, main 05:42 (5f3e51af4), 9 minutes, inside the half hour. Each writer kept its renamed file's handle,
  which shares no deletion, open through the folder's sync, so a second writer of the same name was refused with access
  denied once the sync outlasted its retries (BENEATH-TWO-WRITERS-WINDOWS, now Closed). About 10 product lines added
  and 17 removed, and 18 test lines. Found from a hosted Windows red on a commit that touched no platform code; the
  diagnosis read the cause from the code and the hosted log (14 minutes, before the builder). Proven on hosted Windows
  by two scratch-branch probes with a 1000 ms delay before the folder's sync: the new test and the two-writers test red
  at the parent with the hosted log's error, all 32 beneath tests green with the fix. Where the minutes went: the build
  (2 minutes); the probes (6 minutes, while the gate ran 2 and one review 6, blockers 0).
- No daemon test reaches the live comm folder, 2026-11-10: builder launched 06:25, main 07:14 (102b3b766), 49 minutes,
  past the half hour, a failure by this page's standard. Under test, the one comm-home resolver aborts the run, naming
  the test and the path, when the home it resolves is outside the temporary folder (TESTS-REACH-LIVE-COMM-HOME, Closed).
  Found by REGISTRY-TEMPFAIL-LOCK-TURN's diagnosis, which saw four destroy tests prune and lock the comm folder their
  environment named; the guard then found 31 tests reaching it (destroy 4, reauth 16, create rollback 5, agent
  environment 5, agent argv 3), each now isolated or reading no environment. About 29 product lines and 90 test lines.
  Where the minutes went: the build (28 minutes: the class was eight times the four named, and a blocking task's join
  swallowed the guard's first form, a panic); one review (blockers 0, 10 minutes: three more callers swallowed a
  panic); one fix round (6 minutes: the guard aborts, which deleted the re-raising helper and its three call sites,
  read by the session); two scoped gates of 2 minutes. The cause: the brief sized the class by the diagnosis's four
  tests, and a guard that panics can be caught.
- Every test that takes the comm registry lock holds its turn guard, 2026-11-10: builder launched 07:15, main 07:29
  (aceaebe3a), 14 minutes, inside the half hour. The lock's in-process turn is shared by the whole test process and its
  wait counts inside a test's bound, so other tests' holds spent a 1 second bound on a slow runner
  (REGISTRY-TEMPFAIL-LOCK-TURN, Closed): every lock-taking test now holds the guard, for write when its assertion rests
  on a bound under 10 seconds, and the shared test home waits the product's 10 seconds. No product lines; about 40
  test and record lines. The diagnosis (58 minutes, before the builder) reproduced the hosted red on Linux by slowing
  file syncs: red 10 of 10 at the base, green 9 of 9 with the fix. Where the minutes went: the build (10 minutes,
  applying the diagnosis's diff by hand where the comm-home guard had moved code, the check red at main and green with
  the fix); one scoped gate of 2 minutes; no review (test-only, the diagnosis's diff read by the session).
- The comm-home guard's fix forward, 2026-11-10: builder launched 07:37, main 07:57 (eb2b76a19), 20 minutes, inside
  the half hour. The guard's landing (07:14) turned main red on all three hosted systems and in the unscoped local
  gate: its search for tests reaching the comm folder had set `HOME` inside the temporary folder, so tests that find
  the folder through `HOME`, or `USERPROFILE` on Windows, were missed. One such test on Linux and macOS (found by
  running each of the 1,526 unit tests in its own process with the guard printing instead of aborting, under both
  settings) and three on Windows (found by reading) are isolated; about 5 test lines. Main was red from 07:14 to this
  landing. Where the minutes went: the build (15 minutes); the unscoped gate (3 minutes, `ALLDONE`), run instead of the
  scoped one because the scoped gate had passed the guard (GATE-SCOPED-MISSES-SHARED-CALLEE noted, then closed: a
  diagnosis found the scoped gate ran the same job and the test was a race, COMM-GUARD-RACES-ENV); no review (test
  isolation only, read by the session).
- The window's per-user settings in platform's config folder (WINDOWS-SETTINGS-NO-DURABLE-HOME), 2026-11-10: found
  13:19 while the maintainer used the Chat drawer on a Windows window at 0.7.1-rc.3 (the window read no settings file,
  so Enter never posted); builder launched about 13:27, main 14:14 (227a75877), about 47 minutes, a miss of the half
  hour. About 25 product lines (discovery takes platform's resolver; the Windows launcher's default deleted), 80 test
  lines, and the manual's discovery order stated once. Where the minutes went: the build (8 minutes); review round 1
  (blocker: a new test that would fail on Windows, where the resolver reads neither `XDG_CONFIG_HOME` nor `HOME`);
  the amended commit's scoped gate failed (an edited skill whose project copy was not edited, which must stay
  byte-identical); review round 2 (the same copy, two unpinned rules); three scoped gates of 2 to 3 minutes. The
  Windows-only test first ran in the hosted run on main, green.
- The preview takes an agent's path in the tree's spelling and marks a refused preview (PREVIEW-PDF-STALE),
  2026-11-10: reported by the maintainer 13:24 ("pdfs not updated in preview when new pdf is generated"); diagnosis
  13:2x to 13:50 (an agent badged `$PWD/../x.pdf`; the window kept the `..`, the daemon refused it, the pane kept the
  older bytes unmarked, and the live refresh never matched the watcher's id); builder launched 13:52, main 14:47
  (1cb38101d), 55 minutes from the builder and 83 from the report, a miss. About 95 product lines and 130 test lines:
  one fold at the window's three ways in, a refused preview titled as such, and a badge that a change event re-fetched
  during its load now clears (found by review round 1 and taken into the same landing). Where the minutes went: two
  test rounds the session asked for before review (the wiring and the live refresh pinned through the native State
  harness, run on a second computer under a virtual display); review round 1 (blocker: a row switch restored the
  stale view unmarked); review round 2 (no blockers; three small fixes); four scoped gates of 2 to 3 minutes.
- P14, a filing may name its expected row, 2026-11-10: builder launched about 13:34, main 15:10 (be8ebb740), about 96
  minutes, a miss. About 100 product lines (the filer and the inbox) and 230 test lines. Where the minutes went: the
  build (29 minutes); review round 1 (blocker: daemons from 0.7.0 on ignore the new field, so a forwarded or offered
  request would be filed as an ordinary one; should-fix: two 2 s lock waits in series against a 3 s forward bound, and
  a stuck inbox holding the whole registry lock); the architect's two departures from the plan of record (a request
  naming its row is admitted only by a daemon that appends to the folder itself, never forwarded or offered, the
  forwarding code deleted; the inbox lock first, then the registry lock, within one deadline), with the plan amended;
  review round 2 (no blockers; a test's turn lock and a doc comment); three scoped gates.
- The Chat drawer asks for the posting name once and saves it (CHAT-NAME-ASKED-ONCE), 2026-11-10, three landings:
  the settings write (5e9083a56, 15:44), the prompt's state and foot (245e1cb2b, 15:45) and its keys and save
  (b682e8940, 15:47). Builder launched 14:15, the last on main 15:47, 92 minutes, a miss. About 146, 77 and 90 product
  lines; the first is over the 120-line rule with its strict check, kept whole because it has no caller. Where the
  minutes went: the first build came to 217 product lines in one commit and was split; review round 1 of each part
  (no blockers; the write could leave a file the daemon's strict reader refuses, which would stop folder trust, now
  checked with the `toml` crate before writing; the prompt outlived a row switch, a hide and a reconnect); review
  round 2 of both (no blockers; three small fixes; the prompt split again at the reviewer's seam); the third part's
  whole-scope gate went red in a rows test the window cannot reach (ROWS-ROLLBACK-TEST-RACE), and it was gated again on
  its own change, green.
- P13, an installed tool runs every part it sources, whatever their number, 2026-11-10: builder launched about 15:37,
  main 15:49 (cf9d0f596), 12 minutes, inside the half hour. 20 test and record lines, no product code; read by the
  session, no review (test only); one scoped gate.
- The Chat drawer follows a row switch at once (CHAT-ROW-SWITCH-LAGS), 2026-11-10: reported by the maintainer about
  13:40; diagnosis to 14:14 (the window dropped a channel's watch at a row change and asked again only after its own
  one-second pace and the next idle frame); builder launched 14:15, main 16:03 (333e9430b), 108 minutes from the
  builder, a miss. About 80 product lines (the last two channels' records kept across a switch, the short asks, the
  chat's next ask joined to the window's idle deadlines) and about 600 test lines, including a fixed-seed walk of
  12,000 orders over three channels (about 4 s). Where the minutes went: four review rounds, the first three each with
  a blocker (a change event between the take-up and the snapshot; a recovery restarted before its snapshot; records
  held before a channel's first whole state counted as history, found by the strongest reviewer), the walk added to
  close the class; the fourth round no blockers, its model fix taken; five scoped gates.
- P16, one reader for every inbox's lines from one sender, 2026-11-10: builder launched about 16:45, main 17:43
  (f4f94b9b5), 58 minutes, a miss. About 55 product lines (the reader, the complete-line rule moved into it from the
  cursor) and 72 test lines. Where the minutes went: the build (13 minutes); review round 1 (blocker: an inbox file or
  folder that could not be read gave a partial or empty answer, the skipped inbox its lock rule exists to prevent, which
  would let the chat's rebuild file a post twice; should-fix: two copies of the complete-line rule, the lock-fault rule
  untested); review round 2 (no blockers; the folder rule and "creates no inbox" unpinned, its two tests taken, landed on
  the builder's reds without a third round); three scoped gates.
- P15, one poll prints a bounded batch, and the Stop hook blocks again after it, 2026-11-10: builder launched about
  16:22, main 17:56 (fc88d046c), 94 minutes, a miss. About 30 product lines (the poll's byte budget and its count line,
  the hook's mark of total and unread) and 110 test lines. Where the minutes went: review round 1 (blocker: the hook
  keyed on the inbox total let a turn end with mail unread after a partial poll); review round 2 (no blockers; the
  records' limit unstated, a stale bound, the tick rule in six places, the mark's total half unpinned); review round 3
  (no blockers; two stale comments and the limit sentence too narrow, landed on the session's read of the records diff);
  a rebase onto P16, clean; four scoped gates.
- B3a, the chat's owner runs from the service's start, 2026-11-10: builder launched about 16:45, main 18:30
  (b3b1f7728), 105 minutes, a miss. About 65 product lines (`Chat::boot`, the one composition for the daemon and the
  rig; `begin` private) and 80 test lines. The product code was judged right in every round; the minutes went to its
  tests: review round 1 (blockers: the test never took the daemon's boot path, and its log check raced tracing-core's
  call-site cache, 13 of 20 runs failing beside one sibling); review round 2 (blockers: the owner test watched before
  the socket's hello, 3 of 20 failing; the log check bypassed platform's `test_log`, which already was the one capturing
  subscriber); review round 3 by the strongest reviewer (no blockers, 385 runs without a failure; two record lines);
  four scoped gates.
- B3b, one bounded, marked mail body per channel post, 2026-11-10: builder launched about 17:15, main 18:40
  (2d4eb9dcb), 85 minutes, a miss. About 105 product lines (`mail.rs`; the Slack id grammar made config's one copy) and
  280 test lines. Where the minutes went: before review, the id grammar moved to its owner and the push sweep stopped
  the maintainer's first name, copied into tests from the plan's example; review round 1 (no blockers; 20,000 hostile
  records fuzzed, no forged line; the cut halved the text, an omitted record read as an empty post); review round 2 (no
  blockers; a files-only body could pass the bound by four bytes, the line-end cost unpinned); review round 3, narrow
  (no blockers; 115,000 bodies matched an oracle written from the rules; two tests); two rebases; three scoped gates.
- B3f, the scan read and no thread cursor under a lower bound alone, 2026-11-10: builder launched about 18:11, main
  18:46 (4d4579730), 35 minutes, a miss. About 125 product lines (the scan shares the drawer's request builder and page
  parser) and 430 test lines; closes SLACK-REPLIES-CURSOR-DROPS. Review round 1 (no blockers; every drawer read
  compared before and after, identical but for the refused form; a replies scan kept its parent outside the bounds;
  reds by `unwrap`), its fixes landed on the builder's reds; two rebases; two scoped gates.
- B3e, a channel's boundary file and its filed set in memory, 2026-11-10: builder launched about 18:41, main 19:04
  (5ea47a681), 23 minutes, inside the half hour. About 77 product lines and 134 test lines. Review round 1 (no blockers;
  20 of 21 mutants killed; the filed set's trim at a start dropped filings above the boundary, which the next pass would
  file again, so the bound became soft; `contains` now true at or below the boundary), its fixes landed on the
  builder's reds; two scoped gates.
- B3d, a delivering configuration of at most 32 channels, each with its own floor, 2026-11-10: builder launched about
  18:51, main 19:10 (75242faa4), 19 minutes, inside the half hour. About 56 product lines and 100 test lines. Review
  round 1 (no blockers; two floor mutants survived, the malformed value carried no ordinal, the resolution carried the
  hub's value), its fixes landed on the builder's reds; two scoped gates.
- P18, one chat listener per comm folder, 2026-11-10: builder launched about 18:32, main 19:13 (d22e6c096), 41
  minutes, a miss. About 110 product lines (the lock in the comm folder's `chat/`, a held start tried again each minute,
  the step-down) and 200 test lines. Review round 1 (no blockers; ten mutants killed; a hub move probed end to end; a
  held start gave up for good on a topology being edited, now only on one naming another hub), its fixes landed on the
  builder's reds; two rebases by the session; three scoped gates.
- B3c, the watch model hands every new record of a delivering channel over, 2026-11-10: builder launched about 19:17,
  main 19:36 (2fe137150), 19 minutes, inside the half hour. About 35 product lines and 70 test lines. Review round 1
  (no blockers; 9 of 12 mutants killed; a separate take of the arrived records could be forgotten), and the session
  chose the reviewer's simpler shape: the pump and the start return the handover, which the caller must use, and the
  separate take, its buffer and its stop check went; one rebase; two scoped gates.
- B3h1, the recovery traversal's channel walk and its horizon, 2026-11-11: builder launched about 07:12, main 08:04
  (4fdbd1889), 52 minutes, a miss. About 98 product lines of code (139 with comments), the fake Slack's history pages
  (98) and 335 test lines; the thread walk is the next landing, split before the build. Where the minutes went: review
  round 1 (no blockers; nine mutants; the horizon's taking from the first page alone, the step back by a page's raw
  oldest timestamp and one skipped pace were unpinned, and the contract let a caller commit before the thread walk);
  two narrow rounds on the pace test, whose first gap carried the first connection's handshake, state the test does not
  own; a gate whose results were complete but whose wrapper hung on a user service manager another agent had broken,
  read by hand; two scoped gates.
- An SVG's image reads no file or URL, 2026-11-11: builder launched about 08:33, main 09:03 (fe6a459a7), 30 minutes,
  at the half hour. About 19 product lines and 100 test lines; closes SVG-IMAGE-HREF-READS-LOCAL-FILES. It follows an
  in-process hardening of the SVG reader that did not land after three review rounds, each finding new ways a file
  could still hold the window; the plan now draws a file's SVG in a bounded child process, and this is its first
  landing. Review round 1 (no blockers; 49 hostile spellings traced at the system-call level read nothing, against 42
  reads and 5 hangs at the base), its fixes (one table over four elements and three spellings) landed on the builder's
  reds; two scoped gates.
- The chat drawer's look, L1: Slack's dark colors, the header, the origin badge, the mark band and the selection,
  2026-11-11: builder launched about 08:12, main 09:06 (1434b66a9), 54 minutes, a miss. About 163 product lines added
  and 122 deleted, 368 test lines added. Where the minutes went: review round 1 (no blockers; the manual
  called the lighter band darker; the run finder moved without a test; the frame's order of layout and rectangles was
  untested, so the layout now returns the frame's rectangles and no order remains; the badge's line height unpinned);
  two amend rounds; one rebase by the session; two scoped gates.
- The chat drawer's look, L2: message groups, the gutter and the group's tile, 2026-11-11: builder launched about
  09:08, main 10:04 (7500bbf6e), 56 minutes, a miss. About 171 product lines added and 62 deleted. Where the minutes
  went: a scoped gate red on one host and green on another, a test whose expected eviction sat at a font's wrap point
  (fixed by visiting every scroll position and widening the test's body by the gutter); review round 1 (no blockers;
  33 mutants of the grouping and lead rules killed; the tile's scale, color and fit unpinned; a header-only record's
  tile ran into the next; a page joining above moves every record below it); three amends; two rebases by the
  session; three scoped gates.
- One update runbook for an agent, in `docs/`, 2026-11-11: asked for about 07:40 (a one-line message that
  sends any computer's agent to it, which "needs to work and be foolproof"), main 10:34 (808f2c6d1), a records landing
  of about 180 runbook lines. Where the time went: each round of real runs found a way an update would fail for a
  person, and each fix needed another run. Five hosted probes on macOS and Windows and container runs on Linux, from
  every 0.6 and 0.7 start tried: GitHub's listing refused anonymous calls from a shared address (the newest release is
  now read from the latest-release redirect), the installer enrolled the folder it was run from (the command moves
  home first), Git Bash had no form. Runs of an agent inside a real 0.6.6 row found that the update ends that agent
  mid-run while finishing, that its row starts a new conversation, and that with a live REPL the installer asked on the
  row's terminal and waited forever (the command now gives it none; INSTALLER-ASKS-A-ROWS-TERMINAL). Three review
  rounds (blockers: ending every agent without asking; the in-row path unrun; a shared home skipping the question; an
  older skill triggered by the message's words; the terminal hang; a contract naming a test that does not exist). One
  miss of the session's own: a probe script was edited while a run read it, which lost one case's mail check.
- The chat drawer's look, L3: day dividers, the time alone in the header and Slack's thread summary, 2026-11-11:
  builder launched about 10:37, main 11:27 (4e3954a1e), 50 minutes, a miss. About 106 product lines added and 20
  deleted (the plan said 80; the divider's lead line moved line indexes in existing tests). Where the minutes went: the
  session moved to another account between the build and the review, which ended the builder, and its fix list first
  reached the reviewer by the builder's old name; review round 1 (no blockers; 13 mutants killed; widths 30 to 700 px
  at four scales and midnight in every half-hour offset held; a marked record could count as in view with no band
  drawn below a divider); one amend by a new builder; two scoped gates. The divider is centered in the text's width,
  half a gutter right of the drawer's middle, as recorded in the contract.
