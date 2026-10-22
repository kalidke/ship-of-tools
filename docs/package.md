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
- `sot-capsule`, one supervisor per row and one leg under it running the agent on a real terminal, recorded into an
  append-only voyage. Rows outlive the window and the daemon by design; the last window's lease decides whether a
  computer's rows end when it closes.
- Julia children of the daemon: a kernel per row (Julia-aware previews and project scans; it never runs user code and
  never loads the user's environment), a REPL per row (runs user code in the row's environment), one Pluto server.
- Other computers: `hosts.toml` declares them and names the hub; the window reaches a far daemon over ssh into
  `sotd stdio-bridge`. Sessions message each other through the comm folder `~/.sot-comm`.

## Subsystems

What each guarantees to the rest. Sizes are `scripts/four-numbers.jl` (version 2) at a25f3c39f: product and test tokens
(characters / 4, comments included; integration tests assigned by file), names implemented twice, and cross-subsystem
module references (the baseline for the fourth number until each subsystem is its own crate). A pass loads the package
page, every contract and one subsystem whole; that leaves room for about 200 thousand tokens of code and tests.

| subsystem | guarantees to the rest | product k | test k | twice | cross | fits |
|---|---|---:|---:|---:|---:|---|
| platform | one answer per machine fact; durable publish, kernel locks, close-on-exec births, the peer challenge | 82 | 69 | 43 | 8 | yes |
| wire | every byte between our programs: one frame codec, one op table, one protocol integer | 42 | 15 | 3 | 2 | yes |
| topology | hosts.toml read one way; one ssh recipe; a link gate that ends logins on close | 50 | 54 | 31 | 23 | yes |
| containment (now in lifecycle) | every child in a kill domain its owner ends; every wait bounded | | | | | yes |
| lifecycle | the last window's lease decides whether rows end; every daemon end is bounded and fires the child signal | 45 | 70 | 37 | 18 | yes |
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
| records | the manual says what is true now | 20 | 1 | 10 | 0 | yes |
| total | | 1,959 | 2,087 | 305 | 404 | |

Outside the table: the vt100 fork (47 + 39, vendored, never loaded in a pass) and the shared test support (26).
The design records (51 ADRs and their index) and the old folder pages live in `docs/history/` and load nowhere.

## Draft contracts, one paragraph each

**platform.** The contract is settled (pass 5a): `rust/platform/CLAUDE.md`. Platform is the crate `sot-platform`,
which depends on nothing of ours, and every other crate depends on it. Guarantees, each with the limit the contract
states: a machine fact (state folder, config folder, host name) has one answer, and `host_name` errs, never guesses; a
publication is durable and never clobbers (the caller flushes its source); a lock is the kernel's; a held folder stays
while its maker lives; a descriptor is born close-on-exec (macOS under a birth lock); a local reply is trusted only
after the challenge; storage exhaustion is known by its native code; a volume is proved before a store opens on it; a
restart is paced; a directory pin opens only a directory, and private means private (both on Unix; on Windows each is
a stated gap). Rests on: the OS. Owns no runtime state. Does not: start processes for anyone but the native launcher it
hands out.

**wire.** The contract is settled (pass 5a): `rust/protocol/CLAUDE.md`, the frames and payloads between our programs
(the lanes' frames are the capsule's, the sidecars' protocols the sidecars'). Guarantees: an envelope is at most 1 MiB
in every reader and writer; a blob is exactly its declared length, and the writer refuses a mismatch before a byte; a
payload grows only by defaulted fields, else the protocol integer rises and hello refuses the mismatch (the server's
check, tested there); a child's output is read by lines lossily, so no byte ends a reader; policy asks
`is_release_build`, never the version string; the shared grammars have one parser. Rests on: serde, the compiler and
platform. Owns: nothing at run time. Its crate keeps an edge to `sot-log` through topology's lane client until pass 5b.

**topology.** Guarantees: hosts.toml has one grammar and one writer (the hub's daemon); a host name outside
`[a-z0-9][a-z0-9._-]*` never reaches ssh; every ssh turns sharing off; closing the link gate ends every login watched
through it; a unit is written only where hosts.toml gives it. Rests on: wire, platform, containment. Owns: the hub's
hosts.toml and the generated relay units. Does not: hold connections (fe-net and the daemon's link do).

**containment (carved out of lifecycle).** Guarantees: every child the daemon starts runs in its own process group
(Unix) or job (Windows) that its owner's release or the process-wide signal kills; the signal, once fired, is
permanent and refuses new starts; every wait has a bound on the OS clock; on Linux a guard subreaper ends every
descendant within `DRAIN_BOUND` of the daemon's end. Rests on: setpgid/killpg, Windows jobs, PR_SET_CHILD_SUBREAPER;
the compiler (clippy's process-spawns group allows starts only through `Signal`). Owns: the registry of contained
trees. Does not: decide when to end anything.

**lifecycle.** Guarantees: a lease is granted only to a peer whose pid, creation time and boot the OS reported at
accept; the start plan is a function of `held.json` and this boot alone; a close stops accepting before it touches a
row, and a backstop exits at `SHUTDOWN_BOUND`; every controlled end takes one terminal with fixed exit codes (0, 1, 2,
75, 78, 101; INT and TERM by the signal). Rests on: containment, rows' end API, server's accept loop. Owns:
`held.json`, the leases. Does not: end rows itself (asks rows).

**server.** Guarantees: one daemon per state root; a socket a live daemon answers on is never unlinked; a connection
is admitted twice (OS peer at accept, hello at first frame) and nothing is served before; one writer per connection;
an op runs off the loop unless named inline, so a ping is answered at once; a peer that cannot drain is dropped.
Rests on: platform (peer identity, locks), wire. Owns: the listener, the roster, the op table. Does not: implement any
op beyond hello, ping and the handoffs.

**rows.** The contract is settled (pass 3): `rust/backend/src/rows/CLAUDE.md`. Guarantees: a row's id survives every
daemon restart and every refresh of its label; a row's comm handle has one answer, `bound_handle`, by construction (the
declared handle is a cell private to `rows::handle`), and no two rows hold one; one guard per row; no second supervisor
over a live one (every start path reaches one launch, which takes the row's fence before it forks); no start begins
once the close has begun; a row's phase follows its current supervisor; a row is reported ended only after the destroy
proof, and lifecycle ends rows only through the end API (`rows::end`). Work-state is rows': one writer path (`sotd
work-state`, each write one step under the registry lock), one reader path (`workspace.list`), and a row waiting on
its person reads red on every system. Rests on: capsule (supervisor, attach), agents (recipe), containment, platform
(fence). Owns: the registry, row tomls, state folders, work-state. Does not: run the agent's terminal (capsule), decide
when rows end (lifecycle asks), or read a screen for mail (messaging's wake). Its crate waits for agents, sidecars,
files, messaging and containment to be crates.

**capsule.** To the rest: a row's terminal survives the window and the daemon; one supervisor per state dir, so rows
never start a second; a viewer (window, headless client, wake) attaches through one client and sees the screen as it
is; rows read the supervisor's end as clean, terminal, contended or storage-held. Inside (current mechanism): an
append-only voyage read forever, codec seams, torn-tail recovery, fsync before publish, ground-gated attach, three
lanes, exit codes 0, 69, 70, 71. Rests on: platform (locks, durable publish, process birth), the OS pty/ConPTY. Does
not: know about rows, mail or windows. Decision D2 decides how much of the inside stays.

**messaging.** The contract is settled (pass 4, decision D1): `rust/backend/src/comm/CLAUDE.md`, written from decision
0031, which wins where they differ. Comm is one implementation, in `sotd`: the daemon runs it in process and every
session runs `sotd comm`, on any computer that shares a home with a daemon host; the commands sessions type
(`comm-send.sh`, `comm-poll.sh` and the rest) are thin wrappers that keep their words and exit codes. Guarantees: a
handle is derived, never chosen (the folder, the agent kind and the box; a row keeps its handle through the self file
the daemon pins), and has one inbox; a send ends in `filed` or `FAILED` and nothing else, `filed` only by the
appender after the line is on disk for a live handle, within 5 s or it fails, with no queue and no second route; only
poll moves a cursor; an idle row with mail is woken by its own daemon, which types one fixed line into a free prompt
and never into a dialog, a menu, a working session or a person's unsent text; a busy session's Stop hook holds its
turn until it reads; users on one computer never see each other; a subagent, or an agent started inside a session,
has no mail identity; every registry write is one locked read-decide-write; comm reads its environment as a value it
is given, never the process's. The on-disk formats are 0.6.6's, so both versions share one comm folder. Rests on:
platform (locks), rows (the row's terminal and declared handle, for the wake), topology (the hub link). Owns:
`~/.sot-comm`, `sotd comm`, the wake. Does not: work-state (rows').

**agents.** To the rest: rows get one argv and environment for an agent kind and account, with folder trust
prepared before the start; the shell tools (launchers, row CLIs, `sot-fe`) reach the daemon only through the wire.
Inside (policy): every Claude leg runs `--permission-mode auto`; a leg starts a fresh conversation unless a reauth or
a person resumes one; trust is written only under the declared prefix; `claude` and `ccx` are found by absolute
path. Rests on: rows (the launch), messaging (handle), distribution (install of bin and skills). Owns: the launch
recipe, launchers, the non-messaging skills, the row CLIs. Does not: drive the window (fe-ui's control surface does).

**files.** Guarantees: node ids are root-relative with `/`; a mutation resolves inside the root or is refused; a write
whose version differs from disk is refused; delete is a trash; the watcher never watches the daemon's own trees nor
crosses a filesystem nor exceeds its budget. Rests on: containment (gio trash), sidecars (the kernel's previews).
Owns: `.concept/`, `.sot-trash/`, the watches. Does not: interpret file content.

**sidecars.** To the rest: per row a Julia kernel (previews, project scans) and a REPL (user code), per daemon Pluto
and MathJax, per monitored host a sampler; a caller submits and waits at most a stated bound and never spawns or
kills; a child's page server answers nothing without a secret. Inside: one task owns each child from spawn to reap;
the kernel never loads the user's environment; each child has its own temporary folder, removed at its end; on Linux
a user-code child has its own X display only it opens. Rests on: containment, platform (held folders), the julia
resolver. Does not: serve pages to the browser (pages).

**pages.** Guarantees: every listener is loopback and the bound port is the one named; nothing served without a grant
(token, nonce, secret then cookie); `.git` and `..` never served; a remote window's page rides its one link and ends
when the link closes. Rests on: topology (link gate), sidecars (Pluto's grant), containment (quarto, git). Owns: the
grant tables (memory only). Does not: render documents itself.

**fe-net.** Guarantees: one task per dialled host; each connect-path reply within 30 s or a disconnect naming the
step; a down ssh host costs at most two logins a minute; a pipe host is leased before its data connection; 90 s of
silence ends a connection. Rests on: wire, topology, platform's `Redial`, lifecycle's window lease. Owns: the
connection set and `session-<host>.json`. Does not: touch view state.

**fe-ui.** Guarantees: daemon events are applied only on the UI thread at the top of a frame, one arm per event kind
with no wildcard; other threads only wake the window; untrusted text reaches the shaper one bidi paragraph per line;
input goes only to the selected row's live client or is counted; showing a result never steals the view. Rests on:
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
old prelude handing off to the first 0.7.0 candidate on a backend-only host with glibc 2.31.

**records.** Guarantees: the manual says what is true now; a design not built is marked unbuilt; no private names.
After the reorganization the package page and the contracts are the only descriptive records a session loads; ADRs
move to a history folder.

## Depends on

Today (crates): platform <- log <- protocol <- {backend, frontend}; platform <- log <- updater <- {backend, frontend} (protocol and the updater
also depend on platform directly; protocol's edge to log is topology's lane client, which leaves with pass 5b). Inside `sot-backend`
(one binary, no library target) the modules form one cycle: {agents clients comm files lifecycle pages rows server
sidecars topology update}; inside the frontend, {lease net pages ui}; inside sot-log, {attach_client capsule lane store
supervisor}.

Target (one crate per subsystem, acyclic; a call the contract does not name does not build):
- platform -> (none)
- wire -> platform
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

## Decisions (settled 2026-10-20)

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
- **D3 sidecars.** The kernel, the REPL, Pluto and the monitor sampler stay; the Linux per-child X display stays (a
  security boundary). The dormant Claude SDK producer (`rust/log/src/claude.rs`, `claude-sdk-helper/`, 47k tokens,
  no product path runs it) is deleted in the sidecars pass. MathJax's node child is decided in that pass on one fact:
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

## Order of passes (session hours and dollars, estimates until each pass is measured)

Work runs continuously, paced by milestones: each pass starts when the previous one lands; the maintainer's iteration on a
contract gates its landing on main, not its start. Rate assumed: about 20 dollars a session hour at high effort (the
package page: 15 minutes, 5 dollars). Each estimate is replaced by the measured figure when its pass lands. A pass
launches only on what loads within 250 thousand tokens; a larger subsystem is split by concept or shrunk by deletion
first, in a short pass of its own. Each guarantee gets one test, red before the change; a pass gets one outside review
at its end and one round for its findings; a class of breaks stops it, and the subsystem is split and run again.

| # | pass | hours | dollars | measured |
|---:|---|---:|---:|---|
| 0 | Mechanical preparation: tests out of product files into sibling files; the 117 folder pages (164k tokens) to one-line pointers, their full text kept in `docs/history/`; the 51 ADRs and their index (215k tokens) to `docs/history/adr/`, loaded by nobody; the root CLAUDE.md to this page; the size-limit and page checks retired; memory pruned to operating state; the four-number script | 3-6 | 60-120 | 0.7 h, about 30-35 dollars; product 1,959 -> 1,962k, tests 2,087 -> 2,034k |
| 1a | Distribution, the release path: `release.sh` without its test re-run or green-at-HEAD rule, one hosted run, the pack; done test: thirty minutes from fixes in hand to a candidate installed on the maintainer's workstation; the outside review | 2-4 | 40-80 | 1.6 h (pass 1.1, gate and landing 0.5); done test 22.4 min by segments; about $23 all-in; distribution 215/295 -> 213/298k |
| 1b | Distribution, install, update and launch (D5) | 6-12 | 120-240 | 10.4 h wall in six runs (the pass 2.9 h; five fix rounds after two outside reviews 7.2 h); about $157 for the pass's agent; distribution 214/298 -> 221/252k, twice 30 -> 56 (one function per system); `install.sh` 2,053 -> 216 lines, `install.ps1` 975 -> 215 |
| 3 | Rows, with work-state and how a row's state is shown | 4-8 | 80-160 | 4.2 h wall in two runs (the pass 3.7 h, its scan follow-up 0.5 h); about $78 for the pass's agent; rows 150/163 -> 162/182k (work-state moved in), messaging 117/285 -> 109/267k |
| 4 | Messaging (D1) | 6-12 | 120-240 | seven agent rounds: the pass 4.8 h (stopped at its boundary), then six short fixes of 1 to 45 min; about $93 for the agents; messaging 109/267 -> 94/124k, twice 20 -> 45 (the shell door's wrappers and generic names, two real pairs queued); comm shell 6,072 -> 2,571 lines, its shell tests 14,272 -> 3,850; about 160k tokens deleted, not D1's 300k (the PowerShell copies were already gone); the local gate 12 -> 6.6 min |
| CI | Distribution's CI: the hosted run at each landing about seven minutes and the local gate near five; the slow proofs to a release-gate run made once per release | 0.5-1 | 10-20 | eight short agent rounds, about 30 agent minutes in all; the rest was hosted runs (nine per-landing, five release-gate) and waits; about $13 for the agents; the per-landing run 48 -> 7.2 min wall measured alone (longest job 6.9 min), the release-gate run 21.5 min; the local gate 12 min, of which the bash 3.2 suites, which messaging deletes, are the wall |
| 5a | Platform and wire: their contracts; platform its own crate out of `sot-log`, its copies folded | 4-6 | 80-120 | 1.2 h wall in three agent rounds (the pass 25 min to a gate-ready head, the review round 6 min after an outside review, a landing fix 8 min), about 40 agent minutes; about $15 for the agent; platform 83/65/47/8 -> 79/80/48/0 (tests rose with the shared test helpers moved in; twice rose by one, generic names such as `context`), wire 42/14/3/2 -> 42/15/3/2; three hosted runs, the landing one 7.9 min |
| 5b | Containment carved out of lifecycle; the lifecycle and topology contracts; topology out of `rust/protocol` | 4-6 | 80-120 | |
| 5c | Server: its contract; the op table into the binary, every service a handler with an explicit context | 4-6 | 80-120 | |
| 6 | Capsule (D2) | 10-20 | 200-400 | |
| 7 | Frontend: State apart first, one short pass per cluster (the row record, focus and prompt, preview requests, the REPL and agent pane), each moving its fields into a type with private fields; then the five crate cuts (D4) as moves: app and render, chrome and input, navigation and session, agent pane and drawer, preview and control | 22-52 | 440-1,040 | |
| 8 | Sidecars (D3) | 4-8 | 80-160 | |
| 9 | Pages and files | 3-5 | 60-100 | |
| 10 | Agents: the launch recipe, launchers and skills | 2-4 | 40-80 | |
| 11 | Release gate: the full hosted matrix once, install on every box, the maintainer's use, the tag (0.7.0); the installers' remaining proof is here: an rc9.15 box's old prelude handing off to the first candidate on a backend-only host with glibc 2.31, and the maintainer's use on the maintainer's own boxes | 3-6 | 60-120 | |

Total: about 78 to 157 session hours, 1,560 to 3,140 dollars, most of the range in the frontend and the capsule.
Wall time is longer by the hosted runs and the maintainer's iterations; a second pass in parallel on a disjoint subsystem (in a
worktree, merged when its contract settles) shortens the calendar, not the work.

## Integration tests: which pass reduces each

Each pass keeps one fast unit test per guarantee in its contract and its part of the one smoke per OS; an
integration scenario that sleeps on the clock goes with the fix it pinned. `rust/backend/tests` (26 thousand lines)
and `rust/log/tests` (27 thousand lines) are assigned by file:
- Distribution 1b: `shell_dial`, `unit_pin`, `same_version`, `window_start`.
- Rows: `capsule_workspaces` (5.7k lines, the row half; the capsule half goes with pass 6).
- Messaging: `comm_wake`, `comm_file`, `comm_send`, `hub_link`, `comm_cli`.
- Daemon core: `daemon_lifetime` (5.6k lines), `window_lease`, `contained_job`, `ancestors`, `admission`,
  `control_session`, `ping_reaper`, `live_socket`, `daemon_boot`, `daemon_stdout`, `subcommand_help`,
  `support_checks`, `active_frontend`, `stdio_bridge`, `topology_set`, `topology_sync`, `relay_refresh`,
  `status_integration`; and from `rust/log/tests` the platform ones: `challenge_unix`, `challenge_macos`,
  `connect_own`, `other_account`, `winhandle_windows`, `macos_kernel_facts`.
- Capsule: `lane_bridge`, `switch_latency`, `keystroke_latency`; from `rust/log/tests` `socket_unix` (5.8k),
  `pipe_win`, `wire`, `e2e_socket`, `e2e_pipe`, `capsule`, `supervisor`, `fault_storage`, `fault_kill`,
  `fault_terminate`, `reconcile_matrix`, `golden`, `conpty`, `fe_client`, `attach_worker`. Under D2 (b) most go with
  the code they test.
- Sidecars: `repl_windows`, `child_folders`. Pages and files: `site_root`, `site_windows`, `preview_order`,
  `old_watcher`. Agents: `trust_declare`, `trust_premises`, `agent_exec`.
- Deleted with the dormant Claude SDK producer (D3): `claude_e2e`, `claude_rig`.
- The shared support folders (`support/` in both) shrink as their users go, in the last pass that uses them.

## State

- The reorganization started 2026-10-20 from main at a25f3c39f, with v0.6.6-rc9.15 published and in use; the
  `release/0.6.6` branch takes fixes found in use, nothing else.
- D1 to D5 are settled (above).
- Step 0 landed 2026-10-20. Pass 1a landed 2026-10-21: the release path's contract is the first settled. Pass 1b
  landed 2026-10-21: the installers' contract is settled, and `sotd install` is the installers' body on every system.
  Pass 3 landed 2026-10-21: rows' contract is settled, with work-state. The CI pass landed 2026-10-21: a landing's
  hosted run takes about seven minutes, and the slow proofs run once per release (`release-gate.yml`, by hand). Pass 4
  landed 2026-10-21: messaging's contract is settled and comm is one implementation, in `sotd` (D1); live sessions keep
  the installed scripts until the 0.7.0 install. Pass 5a landed 2026-10-21: platform's and wire's contracts are
  settled, and platform is its own crate, `sot-platform`, depending on nothing of ours. Pass 5b (containment,
  lifecycle and topology) is next. Nothing is half-done on main.
- Findings go to the owning subsystem's queue and are handled at that subsystem's pass.
