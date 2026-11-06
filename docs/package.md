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
| wire | every byte between our programs: one frame codec, one op table, one protocol integer | 42 | 15 | 3 | 2 | yes |
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
| chat | Slack's message JSON becomes one bounded wire record: identity, origin and bounds kept, edits and deletes told from new messages, no forged own post | 4 | 3 | 0 | 1 | yes |
| records | the manual says what is true now | 20 | 1 | 10 | 0 | yes |
| total | | 1,959 | 2,087 | 305 | 404 | |

The chat row is an estimate from its files' characters / 4 (the script does not yet know the crate), and its row of
the total is not recounted. Outside the table: the vt100 fork (47 + 39, vendored, never loaded in a pass) and the shared test support (26).
The design records (51 ADRs and their index) and the old folder pages live in `docs/history/` and load nowhere.

## Contracts

Each subsystem's contract is the CLAUDE.md named here; it states the guarantees with their limits and tests, and wins
over this page. The table above says what each guarantees to the rest.

- **platform:** `rust/platform/CLAUDE.md`, crate `sot-platform`, depends on nothing of ours; every crate depends on it.
  Rests on: the OS. Owns no runtime state. Does not: start processes for anyone but the native launcher it hands out.
- **wire:** `rust/protocol/CLAUDE.md`, the frames and payloads between our programs (the lanes' frames are the capsule's,
  the sidecars' protocols the sidecars'). Rests on: serde, the compiler, platform. Owns nothing at run time.
- **chat:** `rust/chat/CLAUDE.md`, crate `sot-chat`, on wire alone for now (platform joins with its first use). Owns:
  normalization of Slack's message JSON into the wire's record (representation: wire's). Does not: open a connection,
  hold a credential, file mail or map channels; nothing links it yet but the workspace.
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
since 6c-1a1); protocol <- chat, which nothing links yet; {protocol, capsule} <- topology; the backend depends on
protocol, topology, updater, containment, capsule and platform, the frontend on all of those but containment. Wire
(protocol) depends on platform alone; topology on wire, platform and capsule (the capsule's `Endpoint`); containment
on platform alone. Inside `sot-backend`
(one binary, no library target) the modules form one cycle: {agents clients comm files lifecycle pages rows server
sidecars topology update}; inside the frontend, {lease net pages ui}; inside sot-capsule, {capsule endpoint
row supervisor} (since 6d-1; the local transport went to platform).

Target (one crate per subsystem, acyclic; a call the contract does not name does not build):
- platform -> (none)
- wire -> platform
- chat -> wire, platform (today wire alone)
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
traps, never a load split or a time. An edit past ten minutes is a flag, and a landing past thirty minutes from its
change to main is a failure, stopped and written up with its cause. Each new rule gets one test, red before the
change, and the proof is the build, the existing suite and those tests. A landing's record, with its measured times,
goes to `docs/history/passes.md`, which no pass loads, not to this page; the same file holds the pass table, the
earlier text of the sections above and the integration-test assignment by pass.

## State

v0.7.0 is published (2026-11-05), and a plain install moves a computer to it; `main` carries 0.7.1-DEV, and the
`release/0.6.6` branch is closed to all but fixes found in use. A new release candidate waits until a person has used
the last one and carries everything that use found. In progress is the project chat (the daemon speaks Slack itself,
in Rust, one workspace, a person's post under a custom author): its plan of record is `docs/history/slack-client-plan.md`.
Its first stage's preparations (P2, P4, P10, P11, P5, P6, P9, P1) and B1a, the record and normalization, have landed;
the stage continues with B1b, B1c, B1d, B2, then B5, each landing alone within the half hour. Figures (B4 with P3, P7, P8) and mail (B3 with P14, P13) wait for the
maintainer's word after the first stage is measured; SlackClaw's pause switch (its PR 18) merges on his word when the
first channel moves. Queued, not started: the installer-observation pass (`pass/install-observe`), 26.04 runner support,
and each subsystem's findings in `docs/defects.md`, handled at its pass.
