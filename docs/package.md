# Ship of Tools, the package

What it is: an agentic development environment for Julia. Agent programs (Claude Code, Codex) run in rows, one
project folder each, on any of the lab's computers; one native window steers, watches and reviews them, browses the
project, previews files and runs a Julia REPL. A daemon per computer and OS account owns that computer's rows and the
processes that serve them, and the window holds one connection to each daemon it dials.

## How it runs
- `sot`, the window (Rust, winit + wgpu). ratatui computes the chrome's cells only; one wgpu pass draws every pixel.
  It is not a terminal program. It holds one connection to each daemon it dials.
- `sotd`, the daemon, one per OS account per computer. It owns that computer's rows, serves files, previews and pages,
  supervises the Julia children, and answers on one private socket (a named pipe on Windows).
- `sot-capsule`, one supervisor per row, running the agent on its own terminal (a pty, ConPTY on Windows), keeping a
  bounded screen file and serving the row's one socket. Rows outlive the window and the daemon by design; the last window's lease decides whether a
  computer's rows end when it closes.
- Julia children of the daemon: a kernel per row (Julia-aware previews and project scans; it never runs user code and
  never loads the user's environment), a REPL per row (runs user code in the row's environment), one Pluto server.
- Other computers: `hosts.toml` declares them and names the hub; the window reaches a far daemon over ssh into
  `sotd stdio-bridge`. Sessions message each other through the comm folder `~/.sot-comm`.

## Subsystems

What each guarantees to the rest. Sizes are `scripts/four-numbers.jl` (version 2) at a25f3c39f: product and test tokens
(characters / 4, comments included; integration tests assigned by file), names implemented twice, and cross-subsystem
module references (the baseline for the fourth number until each subsystem is its own crate). A pass loads the package
page's paragraphs it touches, the contracts of the subsystems it edits and of those that cite them, and its own code
and tests whole; Order of passes says how a load is counted.

| subsystem | guarantees to the rest | product k | test k | twice | cross | fits |
|---|---|---:|---:|---:|---:|---|
| platform | one answer per machine fact; durable publish, kernel locks, close-on-exec births, the peer challenge | 82 | 69 | 43 | 8 | yes |
| wire | every byte between our programs: one frame codec, one op table, one protocol integer | 47 | 20 | 7 | 1 | yes |
| topology | hosts.toml read one way; one ssh recipe; one login per computer for the window, its end ending every channel | 50 | 54 | 31 | 23 | yes |
| containment | every child in a kill domain its owner ends; every wait bounded | 26 | 18 | 27 | 3 | yes |
| lifecycle | the last window's lease decides whether rows end; every daemon end is bounded and fires the child signal | 22 | 44 | 21 | 14 | yes |
| server | one listener; nothing served before the OS peer check and an admitted hello; one writer per connection | 52 | 121 | 14 | 42 | yes |
| rows | a row's id survives restarts; one guard per row; no second supervisor over a live one | 128 | 145 | 38 | 41 | no |
| capsule | a row's terminal outlives the window and the daemon; every viewer attaches through one client | 414 | 469 | 140 | 2 | no (4x) |
| messaging | `filed` only when the line is appended; a wake for an idle row; one writer per registry | 139 | 308 | 22 | 22 | no (2x) |
| agents | rows get one argv and environment per agent kind and account, with folder trust prepared first | 137 | 79 | 10 | 12 | borderline |
| files | a workspace's files confined to its root; versioned writes; delete is a trash | 37 | 19 | 6 | 48 | yes |
| sidecars | per row a kernel and a REPL, per daemon Pluto and MathJax; callers submit and wait a stated bound, never spawn | 106 | 119 | 42 | 34 | borderline |
| pages | nothing served that an op did not grant; loopback only; a remote window reaches pages over its one link | 36 | 50 | 12 | 26 | yes |
| fe-net | one connection per dialled host, retried on a pace; typed requests and events, never bytes | 62 | 33 | 22 | 79 | yes |
| fe-ui | one UI thread owns all view state; one wgpu pass draws every pixel | 391 | 194 | 66 | 29 | no (3x) |
| distribution | a box runs exactly one tagged release; install verifies before it writes; launch fails open | 211 | 308 | 30 | 18 | no (2.5x) |
| chat | Slack's message JSON becomes one bounded wire record: identity, origin and bounds kept, edits and deletes told from new messages, no forged own post; the Slack calls (history paged and bound to its query, one post submitted once, reads paced by the retry header, names through a bounded cache) all leave through one HTTPS door that verifies the server, sends the bot authorization only to Slack hosts, and bounds time, admission and size; the Socket Mode listener acknowledges each envelope of its own app before any work and ends on another app's, keeps one socket at a time, and latches a gap for anything it loses | 24 | 27 | 45 | 1 | yes |
| records | the manual says what is true now | 20 | 1 | 10 | 0 | yes |
| total | | 1,959 | 2,087 | 305 | 404 | |

The wire and chat rows are measured by the script at this change (the fake Slack's server code counts as product); the other rows and the total are as measured at a25f3c39f. Outside the table: the vt100 fork (47 + 39, vendored, never loaded in a pass) and the shared test support (26).
The design records (51 ADRs and their index) and the old folder pages live in `docs/history/` and load nowhere.

## Contracts

Each subsystem's contract is the CLAUDE.md named here; it states the guarantees with their limits and tests, and wins
over this page. The table above says what each guarantees to the rest.

- **platform:** `rust/platform/CLAUDE.md`, crate `sot-platform`, depends on nothing of ours; every crate depends on it.
  Rests on: the OS. Owns no runtime state. Does not: start processes for anyone but the native launcher it hands out.
- **wire:** `rust/protocol/CLAUDE.md`, the frames and payloads between our programs (the lanes' frames are the capsule's,
  the sidecars' protocols the sidecars'). Rests on: serde, the compiler, platform. Owns nothing at run time.
- **chat:** `rust/chat/CLAUDE.md`, crate `sot-chat`, on wire, platform and `toml`. `sotd` builds its service on the configured hub alone (`rust/backend/src/chat.rs`), and admits each chat action through one recheck of its row and channel; its seven request ops, `chat.post_file` and `chat.file` among them, are dispatched to it (`rust/backend/src/chat/ops.rs`; the setup's three in `chat/setup.rs`), and the watches and the `chat.changed` and `chat.status` events are served (`rust/backend/src/chat/watch.rs`). Owns:
  normalization of Slack's message JSON into the wire's record (representation: wire's), `http`, the one door to the
  network, the Slack calls, the Socket Mode listener, and the configuration and channel mapping read through platform's
  config folder. Does not: hold a credential at rest, resolve a row, lock a workspace or file mail.
- **topology:** `rust/topology/CLAUDE.md`, crate `sot-topology`; the daemon's half is in `sotd`. Owns: the hub's
  hosts.toml and the generated relay units. Does not: hold the window's or the daemon's connections.
- **containment:** `rust/containment/CLAUDE.md`, crate `sot-containment`, on platform alone. Owns: the registry of
  contained trees and the guard. Does not: decide when to end anything.
- **lifecycle:** `rust/backend/src/lifecycle/CLAUDE.md`. Rests on: containment, rows' end API, server's accept loop.
  Owns: `held.json`, the leases, the terminal. Does not: end rows itself, contain a child.
- **server:** `rust/backend/src/server/CLAUDE.md`. Services reach it only through their handler arguments. Owns: the
  listener, the roster, the op table. Does not: implement any op beyond hello, ping and the handoffs.
- **rows:** `rust/backend/src/rows/CLAUDE.md`. Rests on: capsule, agents, containment, platform. Owns: the registry, row
  tomls, state folders, work-state. Does not: run the agent's terminal, decide when rows end, read a screen for mail.
- **capsule:** `rust/capsule/CLAUDE.md`, crate `sot-capsule`, on platform and wire alone. Does not: know about rows,
  mail or windows.
- **messaging:** `rust/backend/src/comm/CLAUDE.md`, written from decision 0031, which wins where they differ. Comm is
  one implementation, in `sotd`; the commands sessions type are thin wrappers. Rests on: platform, rows, topology.
  Owns: `~/.sot-comm`, `sotd comm`, the wake. Does not: work-state (rows').
- **agents:** `agents/CLAUDE.md`: the launch recipe in `rust/backend/src/agents/`, the agent layer the installer writes
  and the shell tools. Rests on: rows, messaging, distribution. Does not: run the agent's terminal, decide when a row
  starts or ends, read or write mail, drive the window.
- **files:** `rust/backend/src/files/CLAUDE.md`. Rests on: containment (gio trash), sidecars (the kernel's previews).
  Owns: `.concept/`, `.sot-trash/`, the watches. Does not: interpret file content.
- **sidecars:** `rust/backend/src/sidecars/CLAUDE.md`. The REPL announces its browser ports through a capability pages
  owns, so sidecars names nothing in pages. Rests on: containment, platform, the julia resolver. Does not: serve pages.
- **pages:** `rust/backend/src/pages/CLAUDE.md`. Rests on: topology, sidecars (Pluto's grant), containment. Owns: the
  grant tables (memory only). Does not: render documents itself.
- **fe-net, fe-ui:** `rust/frontend/src/net/CLAUDE.md` and `rust/frontend/src/ui/CLAUDE.md`. fe-net rests on wire, topology, platform, lifecycle's window lease; owns
  the connection set and `session-<host>.json`; does not touch view state. fe-ui rests on fe-net, the capsule's attach
  client, winit and wgpu; owns all view state and UI persistence (D4).
- **distribution:** `scripts/CLAUDE.md`: the release path, the installers, the publish guard. Rests on: the release
  tag, `sotd` subcommands, GitHub releases. Owns: `$PREFIX`, the units, the comm install. Proven only at the release
  gate: the Windows converge and an old prelude's handoff on a glibc 2.31 host.
- **records:** the manual says what is true now; a design not built is marked unbuilt; no private names. The package
  page and the contracts are the only descriptive records a session loads.

## Depends on

Today (crates): platform <- {containment, protocol, capsule, updater}; protocol <- capsule (the row socket's frames,
since 6c-1a1); protocol <- chat; platform <- chat; chat <- backend; {protocol, capsule} <- topology; the backend depends on
protocol, topology, updater, containment, capsule, chat and platform, the frontend on all of those but containment and chat. Wire
(protocol) depends on platform alone; topology on wire, platform and capsule (the capsule's `Endpoint`); containment
on platform alone. Inside `sot-backend`
(one binary, no library target) the modules form one cycle: {agents chat clients comm files lifecycle pages rows server
sidecars topology update}; inside the frontend, {lease net pages ui}; inside sot-capsule, {capsule endpoint
row supervisor} (since 6d-1; the local transport went to platform).

Target (one crate per subsystem, acyclic; a call the contract does not name does not build):
- platform -> (none)
- wire -> platform
- chat -> wire, platform
- containment -> platform
- capsule -> platform (and its own store, leg, supervisor, lanes, attach client inside, or the D2 replacement)
- topology -> wire, platform, containment, capsule (the lane client's endpoint)
- files, sidecars, agents -> wire, platform, containment
- pages -> wire, topology, containment, sidecars (Pluto's grant read through a narrow trait)
- messaging -> wire, topology, platform, containment
- rows -> capsule, agents, sidecars, files, messaging (handle binding), containment, platform
- sotd (binary): server, lifecycle, the op table, the wake (needs rows and messaging), update ops -> all of the above
- fe-net -> wire, topology, platform; fe-ui crates -> fe-net, capsule (attach client); sot (binary) -> fe-*
- distribution: scripts, the updater (-> platform), and `sotd` subcommands

The cuts this needs, each a named change in a pass: the op table moves into the binary and every service exports
handlers that take an explicit context (today services call `dispatch` and the server calls services); lifecycle asks
rows through an end API instead of reaching into them; rows owns the declared handle and messaging reads it; sidecars
announces browser ports through a channel pages owns, instead of calling pages; pages takes a window key instead of
the roster's `ClientGuard`.

## Platforms

Same on all three except: containment (Linux: guard subreaper; macOS: only controlled ends end children, the birth
lock; Windows: jobs and console groups); server (Unix: peer uid at accept; Windows: the pipe's owner-only descriptor
and inbound buffer); capsule (pty on Unix, ConPTY on Windows); sidecars (Linux only: the per-child X display and the
local monitor sampler); distribution (sh and systemd on Linux, sh on macOS, PowerShell 5.1 and a launch supervisor on
Windows).

## Decisions in force (settled 2026-10-20)

- **D1 comm is one implementation, in the daemon.** The daemon is the one writer of inbox and registry and the one
  answer to "who am I"; hooks and scripts call one `sotd comm` command. Shipped as 0.7.0.
- **D2 the capsule is a per-row supervisor** that survives the daemon; scrollback is a bounded file (attach carries
  200 rows), so a reboot still shows the old screen; recovery is resume by id. Done test: `/resume` in a row lists and
  resumes that project's earlier conversations; a row survives a window close and a daemon restart; attach shows the
  screen at once. Command-line resumes (`--resume <id>`, `ccb --continue`, `ccx --continue`) are argv to the agent.
  The row socket speaks wire's frames and its hello carries its own row-protocol number, so a daemon update never
  strands a live row.
- **D3 sidecars.** The kernel, the REPL, Pluto and the monitor sampler stay; the Linux per-child X display stays (a
  security boundary).
- **D4 the frontend splits into five crates** along the event router (`drain_events`): app and render; chrome, input,
  help and persistence; navigation and session; agent pane and drawer; preview with the control surface. Held to the
  cross-crate call count (the fourth number). No cut is possible while the window's `State` stands (Rust allows an
  inherent impl only in its defining crate), so short passes first move each cluster of `State` fields into a type
  with private fields and no open mutation, and replace any scan over it; each is judged by whether the next change
  needs less surrounding knowledge. The five crate cuts follow as moves.
- **D5 the installer's logic is in Rust** (`sotd install`); `install.sh` and `install.ps1` fetch, verify and exec it.
- The repo's committed permission settings (`.claude/settings.json`) allow its own build, check, test, lint, gate,
  pack, workflow-run and install commands and reading its releases; nothing in them tags or releases.

## Order of passes (the standard for a landing)

A plan states the change: its rules, the files it touches, and its product and test lines. It never states a duration
or an estimate. The load (every file the change reads or writes, whole and once, `wc -c` / 4, with the contracts it
edits and those that cite them) decides the context that holds the change whole; it never decides the number of
landings, and a change is split only when the change itself is large. A planner prompt asks for the change and its
traps, never a load split or a time. A landing's record gives its minutes and where they went, and no code change takes an
hour (the maintainer, 2026-11-07). Each new rule gets one test, red before the
change, and the proof is the build, the existing suite and those tests. A landing's record, with its measured times,
goes to `docs/history/passes.md`, which no pass loads, not to this page; the same file holds the pass table, the
earlier text of the sections above and the integration-test assignment by pass.

## State

v0.7.2 is published (2026-11-19), and a plain install moves a computer to it; `docs/UPDATE-AGENT.md` is the one update
procedure an agent follows, from any 0.6 or 0.7 install. It is 0.7.1 with two faults on a Mac fixed, cut from `main`
on the maintainer's word with no candidate once the tests had passed: the whole local gate at the commit that was cut,
and, on the last commit that changes product code, the hosted Rust tests and the release gate's hosted run, both with
their macOS jobs on macOS 26 and on macOS 27, and the release workflow's builds and install smokes without a tag,
every job green (the Rust run after a second run of two jobs). The
first fault: on macOS 27 the kernel's TCP table shows a process no other process's connection, so 0.7.1 refuses
every browser there and opens no page (seen by a person on a Mac); the owner lookup now walks the account's processes
(platform's guarantee 6 has the rule and its limits). The second: a window started from the app's icon on a Mac left
its log nowhere; the launch wrapper now keeps it in `frontend.log` beside the daemon's log, with the launch before it,
when macOS started it as the app (distribution's contract, under "Installers", has the rule, its measurement and its
limits),
and an update brings that to an existing install. The hosted Rust tests and the release gate's run now run their
macOS jobs on macOS 27 as well as macOS 26. Proven on hosted macOS 26.7 and 27.0 only: no person's Mac has run
either fix. v0.7.1 was published 2026-11-18. It is rc.14's code under a new version stamp, cut from `main`
the same morning on the maintainer's word after the whole local gate, and published on his word with hand-written
notes once the release workflow's builds, smokes and install smokes had passed and the release gate's hosted run had
ended with 22 of 25 jobs green; its three red jobs were faults in tests, corrected on `main` after the tag and green
there (the pane-timing test looked for the daemon and the capsule two folders above its own binary, which a newer
Cargo no longer puts there; three chat fixtures pressed Ctrl for keys bound to Primary, which is Cmd on a Mac). The
hub moved to it by a plain install the same hour. The manual's `/stable/` on the site is the published release's,
deployed from the branch `stable`, which moves after a publish (it had held the 0.6.6 manual until that day), and
`/dev/` is `main`'s. v0.7.0 was published 2026-11-05. v0.7.1-rc.14 (2026-11-18) is an unpublished draft, cut on
the maintainer's word in a session's pane once everything his use of rc.13 found was on `main`, and installed by archive
by their own sessions on two window computers and by the maintaining session on the hub, where the daemon restarted and
the rows' open REPLs ended, on his word; a third window computer was not reachable. What it carries past rc.13 is listed
below, where this page says what `main` holds of that use. No person's use of it is recorded yet. v0.7.1-rc.13
(2026-11-17) is an unpublished draft, cut on
the maintainer's word in a session's pane once everything built was on `main` (his word that day: a candidate has
everything, and is the release if it works), and installed by archive by their own sessions on two window computers
and by the maintaining session on the hub, where the daemon restarted and the rows' open REPLs ended, on his word. It
carries several Slack workspaces on one hub, one per project, up to 8: a person connects each from the Chat drawer and
picks a project's channel in any of them, no request crosses workspaces, a reload restarts only the workspaces it
changes and no workspace's stop holds another's, and the drawer's title names the workspace when the hub serves more
than one; the Chat drawer's words in plain language; a Socket Mode listener that serves only its own app (an envelope
of another app ends it unacknowledged, `wrong_app`) and a token Slack refuses by its own code ending the chat's service
instead of a retry forever (`app_token_refused`, `bot_token_refused`), with the Connect step saying whose token it was;
a PDF page or a video's poster past the text cap shown whole, up to 64 MiB (a person had seen a zoomed page read
"binary file" on rc.12); and, after a boot whose rows all started within seconds (seen by a session on the hub), the
boot's starts begun 3 s apart in the strip's order with a waiting row listed `queued`, and a session start that waits
up to 40 s for the registry lock. The maintainer used it that evening on the hub (a second workspace connected,
channels picked in both, posts from the drawer, the previewed file posted, a figure posted by a row's agent, all of
which worked), and what that use found is on `main`. v0.7.1-rc.12 (2026-11-17) is an unpublished draft, cut
early on the maintainer's word in a session's pane, because Claude Code 2.1.326 stopped every new Claude leg on a
question about the folder `--add-dir` adds (the comm folder's agent layer) and a leg cannot answer it: the spawn now
records that folder's trust beside the row's, under the same declared prefix (agents guarantee 6;
LAYER-ADD-DIR-TRUST-STOP). It also carries a chat picture opened full size in the preview, its original decoded only in
the window's bounded drawing child, which completes the figures plan. It was installed by archive by its own session on
one window computer and by the maintaining session on the hub, where the daemon restarted and the rows' open REPLs
ended, on his word, and a new Claude row on the hub then started to work with no question. v0.7.1-rc.11 (2026-11-17)
is an unpublished draft, cut on the maintainer's word from his decision card and installed by archive by their own
sessions on two window computers and by the maintaining session on the hub, where the daemon moved off rc.7 and the
rows' open REPLs ended, on his word; a third window computer was not reachable. It carries the chat set up from the window (the Connect step and
the channel picker), a person's post under their own Slack user token (CHAT-PERSON-POST-NOT-ANSWERED stays open: its
premise is unmeasured until a person's first post), a row's post of a file, pictures as tiles in the Chat drawer, a
person's post of the previewed file, and, on Windows, the writers' turn and the reader's retry when a name is replaced;
no person's use of it is recorded yet. rc.10 (2026-11-15) was installed by archive by their own sessions on both window
computers, where the maintainer used it: a frame of a scroll lays out only the text that is new and no blank space, so a
page in a large agent pane eases as the preview's does (his word on it: "the llmpane scroll is even better"). rc.9 (2026-11-15) put the cell panes' page back to rc.6's ease:
the row pace rc.7 added answered a cause that was not the one seen. rc.8 (2026-11-15) carried what his use of rc.7 asked
for: Claude rows start in Claude Code's classic renderer (his word: "we don't want claude fullscreen"), so the window's
eased scroll reaches their conversation; a row's new run starts with the terminal's input modes reset; the preview's
own messages in a box over the preview pane, held 5 s and fading, with plainer words for a refused drawing; and the
nav pane's status pinned above its tree. rc.7 (2026-11-14) carried the Chat title's name for a mapped private
channel. rc.6 (2026-11-14, the maintainer's word that it
was expected to become the release) carried what he asked for that day: an SVG example, the Chat drawer on Ctrl+K, a
line when drawing over a picture that is no image file, the drawer kept per row, the channel's name in the Chat title,
and every text pane's eased scroll. rc.5 (2026-11-14) carried the fixes for the four defects a person had
seen open on rc.4: a copy as one line, the wake holding a row's input before it waits, a parked
attach client that keeps reading with input around an attach never sent blind or dropped uncounted, and the daemon's
unit no longer restarting forever against another daemon; two parts no test reproduced stay open
(RELAY-SOCKET-HANG's long wait, KEYS-DROPPED-WHILE-A-ROW-ATTACHES's window warning). rc.4 (2026-11-11) carried the
preview that follows its row, the pane's copy and the chat drawer's look (rc.3 the drawer; rc.1 and rc.2 selection and
drawing). `main` carries 0.7.3-dev, no release cut from it, all of it landed 2026-11-19 and none of it used by a
person. Of the four things the maintainer asked of 0.7.3 that day, two are built: a drawer's own key cycles it open,
full height and closed, with the status line on a full-height drawer's border, and Up and Down in edit mode move by
the row on screen under a cursor drawn over the text; drawing on a PDF page and selecting its text are not started
(`docs/defects.md`, fe-ui). Also on `main`: a name after `@` in a chat post is a Slack mention, resolved by the daemon
among the workspace's members and told back to the poster by name (the maintainer: "my @person in slack isn't
working"; no person has yet been notified by one, a stand-in answered for Slack); a skill that tells a session how to
post to its project's channel; the REPL drawer in Julia's own colors, its long lines wrapped at the pane's width; the
Windows launcher starting for a user name with a space in it (the maintainer's report); Escape on an edited buffer
drawing its discard question, and a discard showing the file as last saved; typing and selecting in edit mode shaping
only the lines a key changes (under 1 ms a key on a 5,000-line file, release build, where each took most of a
second), with the selection drawn as a band under the text; and the scoped local gate running the tests that read
files outside their own crate. rc.14 holds what a person's use of rc.13
found (2026-11-17 and 18), each seen by him unless marked: the Chat drawer's foot explains how a post appears only until
this computer's first post; a key's letter is written lowercase everywhere in the window; the refusal to draw shows in
the preview's box only; words that follow a person's action stay on the status line 10 s against background writes (the
post key over a picture opened from the chat says why nothing is posted), a link going down shows at once, the
connection line never says connected for a computer whose link is down (a note from building it), a key the row's
program did not
take shows in the agent pane itself, and a preview's own words after a re-render or re-fetch nobody asked for go to the
preview's box (the limit that remains, another program whose words change at each saying, is in fe-ui's contract); a
project's private channel that a newly connected Slack app cannot read says so, with the line to type in Slack, in the
drawer, the channel list and the connected screen, whose list of such channels the daemon reads after the connect and
carries on `chat.workspaces`, and the post-as-you screen names the connected Slack app as the one to keep
and a connect whose codes are not known to be the connected Slack app's writes nothing and asks first, on one screen
where Enter keeps the connected app (CHAT-CONNECT-MAKES-A-NEW-APP: built, its premises about Slack unmeasured, and a
window older than this cannot show the question); the daemon writes no set of codes told to be of two Slack apps, which
it learns by opening the App-Level Token's socket once at the connect (a mixed set had ended a workspace's chat as
`wrong_app`, found in review, never seen by a person); the chat's messages say session and the computer that runs your
sessions, where some said row and the computer that runs your agents; the drawer asks for a chat name only when the post
goes out under one; and the manual's two Chat pages are rewritten for a first-time reader. rc.11 carries what the
maintainer asked of the Chat drawer on rc.10 (2026-11-15): Up and Down ease the view to a marked message out of view,
and Left over an empty draft leaves an open thread, as the nav pane's Left does. The `release/0.6.6` branch is closed to
all but fixes found in use. A
new release candidate waits until a person has used the last one and carries everything that use found, and
everything else known to be wrong (the maintainer, 2026-11-13: a candidate saves nothing for the next one). In
progress is the project chat (the daemon speaks Slack itself,
in Rust, up to 8 Slack workspaces on one hub, each project's channel in its own, a person's post under a custom author): its plan of record is `docs/history/slack-client-plan.md`.
Its first stage's preparations (P2, P4, P10, P11, P5, P6, P9, P1), B1a (the record and normalization), B1b (the HTTP core) and B1c (the Slack calls) and B1d (the Socket Mode listener)
have landed, which closes B1; the stage continues with B2, then B5, each landing alone within the half hour. Of B2, the
configuration, the service, the watch model, the wire family, the hub composition, the per-action recheck, the request ops' dispatch and the watch ops with their
events have landed, and so have the window's network side of the family (fe-net's typed chat requests, replies and
events) and the window's first chat owner (one watched channel kept matched to the daemon, each loss recovered by a watch
and one read), which closes B2. Of B5, the drawer, these have landed: the Chat tenant, the owner in a leaf module, the
watch following the shown row, the set of reads in flight, a gap only after the daemon's slowest snapshot, wire's text
grammar, chat's door escaping every message text (with the hub's hello saying so), the literal projection, and the
view of the held records, newest at the bottom, and the title's words for every outcome (a code's words owned by wire);
a reader scrolled up staying put through arrivals, edits and deletes, and a deleted message kept in place as
`(message deleted)`, and the draft (one per row, within what its post can read back whole, drawn at the drawer's
foot), and the post (Enter posts the draft once under `[chat] name`, only on a connection whose hub's hello says it
posts plain text, and only a reply that proves the post clears the draft), and older pages (up to 256 messages read
back below the snapshot, a hard ceiling of 276 while a reader stays on the oldest), and the message mark (Up and
Down mark one held message whose header is in view), and one thread view (ArrowRight opens the marked message's
thread, read whole up to 156 messages, the channel's older ones giving way to it, never its newest 100; Escape closes
it), and posting into a thread (a draft goes where it was begun, the thread read again after the person's own
post), and selection and copy (a drag selects text inside one message, Primary+c copies it or the marked message's
shown text, never shared with the agent), and the native witness (the real window's chat through its State under a
virtual display), which closes B5's plan; a person's first use of the drawer, on rc.11, waits on the hub's chat configuration. A person's post from the drawer goes out under their own Slack user token, with nothing of the app, so a responder in the channel answers it as it answers anyone (built; that Slack's message for such a post has no `bot_id` is checked at a person's first post). Setting the chat up from the window is built: the daemon's `chat.connect` (three tokens
checked with Slack, the secrets written owner-only, the chat restarted from its files with no daemon restart),
`chat.resolve`'s `unconfigured`, `chat.channels` and `chat.pair` have landed, and the window sends all three: the channel picker the last two (an unmapped row opens it, and so does `chat.setup`, `Primary+Shift+K`), and the Connect step `chat.connect` (a row whose hub answers `unconfigured`, or `Connect a Slack workspace` at the end of the picker's list (`Connect Slack again` for an older daemon's), leads the person through making their own Slack app, named for their login, and pasting its three tokens), and so a computer with no `hosts.toml` is its own hub for the chat. The drawer's Slack look is in progress, in landings: Slack's dark colors, the header (the
bold name, a smaller muted 12-hour time), the badge drawn from the origin alone, the mark's band and the selection,
an author's messages grouped under one header with a tile in a gutter, and day dividers with the time alone in the
header and Slack's thread summary, and Slack's inline styles (bold, italic, strike and code, by the drawer's reading of
Slack's rules, not yet compared with the desktop app), and links, mentions on their slug and Slack's special tokens
shown as Slack shows them, fenced code blocks on one panel and quotes with their bar, and `(edited)` on a message Slack
marks edited (left out of every copy), and people mentioned by name (the daemon looks the names up, and a live
change takes them from its cache), and channels by name (a mapped channel, public or private; any other only when public, else its label or
id), have landed. An `.svg` file now previews as its picture fitted to the pane, and every
SVG that comes from a file (an `.svg` file, a markdown figure, a page's math, the last in one batch per frame) is drawn
in a bounded child of the window's own binary (it bounds its memory, CPU, crashes and inherited descriptors before it
reads, and answers on a private stdout) through one worker thread (one draw at a time, each job tagged with the shown
source's generation, a stale draw stopped at once, waiting input bounded, two strikes per generation) and a runner that
holds it to a 10 s wall; the window's own process draws only the Monitor's chart, its text clamped. A crash of it on a
Windows computer was seen to show no dialog and leave no dump or report (2026-11-12). Every text pane's scroll (the
preview, the Chat drawer, the agent pane, the Terminal and REPL drawers) eases over `[display] smooth_scroll_ms`, 150 ms
by default, the terminals and the REPL a whole row at a time (2026-11-14, on the maintainer's word), each frame of it
shaping only the text that came into view (2026-11-15). A PDF page and a video's poster
are drawn by the computer's own poppler and ffmpeg where the kernel's search path has them, and otherwise by Julia's
copy, which the first preview that needs it fetches once per user per computer in a child process (nothing is
downloaded at install, and no copy is loaded into the kernel). Also in progress,
built alongside the chat: the agent sees the person's selection (`requirements.md`, "The agent
sees what the user selects"); its wire op, the daemon's pure check and the handler, with its replies (`shared`, `held`, `no_agent`, `not_a_window`; the file published or removed per row) and the withdrawal (the file removed at its setting connection's end and by the start-up walk) have landed; the variable naming the row's selection file in each leg's environment is built, and so are the prompt hook that prints it, the marker that says which legs carry the hook and the test of the path from a connection standing in for the window to the hook, the window's network side of the request, and the window's share of a released text selection (in rendered markdown the source lines it was rendered from; sent when it changes,
cleared when it goes, sent again after a reconnect, each reply on the status line), and the manual says how a selection
or a drawing reaches the agent; the done test through a window and the Codex wiring are unbuilt. Figures, on the maintainer's word
(2026-11-16), are built by the plan `docs/history/chat-figures-plan.md` in place of B4: a row's agent posts a file, the drawer
shows a message's pictures and opens one in the preview, and a person posts the previewed file, all in memory, nothing
on disk; all of it has landed: the chat's share of a file (Slack's three upload steps, only the share a mutation), and the share's read-back (an own file share takes its origin and name from its label), wire's two ops, and a row's post of a file of its folder (`sot-fe chat post --file`, through the daemon's `chat.post_file`), which closes the first goal, and a picture's bytes from the channel to the window's network side (the daemon's `chat.file`, its bytes the reply's blob, and fe-net's request for it, bounded by its variant), and the window's pictures owner, which asks for the thumbnails of the pictures the drawer places, one at a time per hub, and holds them decoded within bounds and draws them in tiles, and a person's post of the previewed file (`Primary+u`, under their own user token), and the opening of a picture in the preview (a click on a tile, or `Primary+o` on the marked message; its original decoded only in the window's bounded drawing child, never in the window's own process), which completes the plan. Mail (B3) has landed: the filing that names its expected row
(P14), one poll's bounded batch and the one reader of a sender's lines in every inbox have landed, and so have the chat's
owner running from the service's start (delivery can run with no window open), one bounded, marked mail body per
channel post with its one reader, the scan read (both bounds, never a cursor; a thread continuation under a lower bound
alone refused), each delivering channel's boundary file and its filed set in memory, a delivering configuration of at
most 32 channels each with its own floor, one chat listener per comm folder, the watch model handing every new record
of a delivering channel over once, in order, and the recovery traversal, the channel walk with its horizon and the
walk of every thread with a reply after the boundary (a thread Slack answers `thread_not_found` reads as empty, so a
caller may commit to the horizon from a complete pass), and delivery's
decisions, pure (one queue for live and recovered mail, nothing filed before the start's read, a post counted filed
only when its filing returns so; passes owed and paced, each channel's own backoff, and the boundary's commit, which
never reaches a post still pending), and the mail jobs (the recipient, the guarded filing behind the hub check, the
boundary's write and the start's read of every inbox), and the daemon's live delivery (each new post of a delivering
channel filed to its row after the start's read, one filing at a time) and its passes (posts refused or missed found
and filed once, the boundary written after), and the delivery's end before a step-down or a retirement (its filing in
flight returned, its pass and read stopped), which completes the daemon's delivery, and the row replies (a row's agent
answers in its channel, in the mail's thread, through `sot-fe chat post`, one post per call, exit 3 when the post may
exist; the daemon refuses a person's post from anything but a named window and a row's under any name but its own
handle), so a channel post becomes mail and the row's answer reaches its thread with no window open; nothing runs it
until a person maps a delivering channel on the hub, which no hub does yet;
SlackClaw's pause switch (its PR 18) merges on his word when the
first channel moves. Also in progress: the agent sees what the user draws (`requirements.md`); the wire's drawing has
landed, and so has the daemon's picture (the figure with the person's ink, inside the project), written through
platform's write beneath a root (its guarantee 17, on Linux, macOS and Windows); the daemon makes a drawing the row's selection (one at a time, withdrawn as text is, Claude rows only), and the window's drawing has its pure owner (points mapped onto the base,
strokes, undo, clear and the right-button erase), its raster, drawn over the figure it belongs to and no other, and its
mouse, keys and status lines, each row keeps its own drawing across a row switch, and a blank canvas (Shift+D) takes the
pane over the preview without touching it; the daemon's hello says it takes a drawing, and the window's network side
sends one only on a connection whose hello did; and the window shares the drawing with the row's agent and withdraws it,
each reply on the status line, which builds the drawing for Claude rows; a person has used it through a window (on a
picture, a blank canvas and a markdown file, 2026-11-11, on a release candidate), and Codex rows and PDF pages are
unbuilt. Also in
progress: the daemon test run toward the maintainer's four minutes, and one shared test tool.
The gate runs bash 3.2 per changed suite and its longest jobs first (a whole run about 4 minutes at 20 jobs), and
platform's hold tool (its guarantee 16) works in one process and across processes; the chat watch's, the wake's and the
rows' activation test seams and the wake's absence waits are on it; the spawn suite's fixed wait is gone (about 225
seconds to 29). A Claude row stopped at Claude Code's folder-trust dialog under the declared prefix is restarted once
(REAUTH-TRUST-LOST, which stays open until a real loss is seen recovered). Queued, not started: the
installer-observation pass (`pass/install-observe`), 26.04 runner support, mods (ADR 0051, accepted for 0.7.Z, unbuilt),
and each subsystem's findings in `docs/defects.md`, handled at its pass.
