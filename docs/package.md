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
`is_release_build`, never the version string; the shared grammars have one parser. Since pass 5c it also holds what the
services share for replies (`sot_protocol::reply`: the handler output, a bounded frame write, and `reject`, which is not
bounded) and the handoff byte pump (`sot_protocol::pipe`). Rests on: serde, the compiler and
platform. Owns: nothing at run time.

**topology.** The contract is settled (pass 5b-2): `rust/topology/CLAUDE.md`, the crate `sot-topology` (on wire,
platform and the capsule's lane client); the daemon's half stays in `sotd` until 5c's op table. Guarantees, each with
the limit the contract states: hosts.toml has one grammar and one search order, and its writers exclude one another
through one lock, so neither `topology.set` nor `sync` replaces a newer version; a name that reaches ssh from the Rust
tree is checked against `[a-z0-9][a-z0-9._-]*`, and no shell runs on this end; every ssh the Rust tree starts turns
sharing off (the shell bridge is a named exception); a login the window or the lane client starts is admitted through
the link gate, whose close ends the login a dial waits on (a parked spare ends when it is next used); a lane dial is
bounded and typed; endpoints are derived by one function each; a unit sotd writes starts only where hosts.toml gives
it; every `systemctl` call has a wait budget; `sotd stdio-bridge` adds nothing to the stream. Owns: the hub's
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
lags. Services reach the server only through their handler arguments: the compiler enforces the server's private
submodules, the rest is convention until the services are crates. Owns: the listener, the roster, the op table. Does
not: implement any op beyond hello, ping and the handoffs.

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
is; the supervisor says how it ended by its exit code (0 clean, 69 terminal, 70 contended, 75 the agent could not
start, 76 the agent ended by itself), and rows' watchdog owns the one restart budget. Inside (after passes 6a and 6b):
one process per row: the supervisor runs the agent on its own pty (ConPTY in a kill-on-close job on Windows), so the
agent dies with it (macOS limit: an agent that ignores SIGHUP can outlive a SIGKILLed supervisor, no parent-death
signal there); the leg, `sot-capsule run`, the probe, the parent-death lease, the anti-flap counter and the operation
journal are gone (the fence and the pointer stay); a requested end is written on the voyage before it is answered, and
a Stop is not an end; the screen is kept in a bounded file (`<state_dir>/screen`) and shown above a restart line;
input whose acknowledgement was lost is reported as delivery unknown, never sent again. Since 6c-1a1 each supervisor
also serves a row socket (`row-<h>`): wire's frames, a hello carrying its own row-protocol number (the one
compatibility gate between a row and any client), at most 8 connections (a slot is released only when both of its
threads have ended), a bounded request queue that refuses input as busy, a watcher that cannot type, a resize
answered after the OS applied it, and input whose reply timed out reported as delivery unknown and never written into a
later run. Nothing uses it until 6c-1a2; the SOM0 lane and the other two remain until 6c-1b. Rests on: platform
(locks, durable publish, process birth), wire (frames), the OS pty/ConPTY.
Does not: know about rows, mail or windows. Its contract is written in pass 6d; the plan is
`dev/output/reorg/passes/6-capsule-plan.result.md`.

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

Today (crates): platform <- {containment, protocol, log, updater}; protocol <- log (the row socket's frames, since
6c-1a1); {protocol, log} <- topology; the backend depends on
protocol, topology, updater, containment, log and platform, the frontend on all of those but containment. Wire
(protocol) depends on platform alone; topology on wire, platform and log (the capsule's lane client trait); containment
on platform alone. Inside `sot-backend`
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

## Order of passes (session hours and dollars, estimates until each pass is measured)

Work runs continuously, paced by milestones: each pass starts when the previous one lands; the maintainer's iteration on a
contract gates its landing on main, not its start. Rate assumed: about 20 dollars a session hour at high effort (the
package page: 15 minutes, 5 dollars). Each estimate is replaced by the measured figure when its pass lands. A pass
launches only on what loads within 250 thousand tokens; a larger subsystem is split by concept or shrunk by deletion
first, in a short pass of its own. A load counts every file the pass reads or writes, whole and once (`wc -c` / 4),
beside the package page and the contracts (about 60k), so about 190k of code and tests. Files the pass deletes
without reading are not counted. A plan sizes its passes the same way: the capsule plan sized all of 6c at 160k, by
the parts it expected to edit, and counted whole, 6c-1 alone is about 540k. Each guarantee gets one test, red before
the change; a pass gets one outside review
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
| 5b-1 | Containment carved out of lifecycle into its own crate; lifecycle's contract (5b split by the load rule: 211k of code and tests) | 2-3 | 40-60 | 1.0 h wall in two agent rounds (the pass 23 min to a gate-ready head, the review round 5 min after an outside review), about 30 agent minutes; about $11 for the agent; lifecycle 42/67/39/15 -> 22/44/21/14, containment (new) 23/21/22/3; the landing run 12.2 min |
| 5b-2 | Topology: its contract; its own crate out of `rust/protocol`, which ends wire's edge to `sot-log` | 2-3 | 40-60 | 1.0 h wall in two agent rounds (the pass 21 min to a gate-ready head, the review round 5 min after an outside review), about 28 agent minutes; about $9 for the agent; seven defects fixed red first (unserialized `topology.set`, a sync that could overwrite a newer hub edit, three unchecked names reaching ssh or a socket path, a monitor label repeated across headers, a refused edit swallowing a hand edit); topology 50/52/35/26 -> 52/55/35/23, wire 42/15/3/2 -> 41/15/3/1; the landing run 6.9 min |
| 5c | Server: its contract; the op table into the binary, every service a handler with an explicit context | 4-6 | 80-120 | 0.9 h wall in two agent rounds (the pass 22 min to a gate-ready head, the review round 7 min after an outside review), about 29 agent minutes; about $7 for the agent; the op table was already one table and handlers already took explicit arguments, so the cut was the shared reply helpers and byte pump moved to wire, the server's submodules made private, pages and the updater given a closure instead of the roster; queue item 21 fixed on the real start path and the reaper fixed under event load, both red first; server 54/119/15/44 -> 52/118/14/45, wire 41/15/3/1 -> 43/17/4/1; the landing run 9.3 min |
| 6 | Capsule (D2), in passes from the plan: 6a voyage out, 6b one process per row, 6c-1 the row socket in four landings (a1 the socket served beside the old attach lane, a2 the window, wake, topology and bridge switched with the one protocol raise and the single dial, b the daemon's calls on the socket and the old lanes out, c rows on the socket: the adopted-row watch, run identity, the wake's debt per run), 6c-2 the 0.7.0 retire, the dormant drawer and messaging's voyage handle form, 6d the transport home and the contract | 10-20 | 200-400 | 6a: 1.4 h wall (07:43 to 09:10) in four agent rounds (the pass 34 min to a gate-ready head; one review round 8 min; two landing fixes, a test-only re-export the normal build could not see and two screen tests that read only the visible screen on Windows), about 45 agent minutes; about $35 for the agent; capsule 410/403/143/3 -> 341/345/135/3 (69k product and 58k tests deleted); the landing run 6.9 min; 6b stopped at its 45-minute line at 10:10 (49 min, 8 of the rows suite red: a plan gap at the rows boundary, not size), its capsule half kept on its branch; the amendment 7 min, $1.19; 6b-0 (the rows suite reads only the surface that survives 6b, no product code) 2.3 h wall (10:27 to 12:47) in two agent rounds (the pass 16 min to a gate-ready head; one review round 18 min after an outside review that found four tests that would still break on 6b and a pid-reuse race in the new kills, which now go through a pidfd), about 38 agent minutes; about $8 for the agent; rows 162/182/40/45 -> 162/181/40/45, capsule 341/345 -> 341/346; the wall was mostly hosted runs queued behind two fixes on main; the landing run 9.0 min; 6b (one process per row) landed 17:11: the first run stopped at its 45-minute line (50 min, about $14) on five design gaps found after the plan, an 18-minute read: the watchdog's restart budget (the deleted anti-flap counter had been what turned an unlaunchable agent terminal), the rows integration suite left out of the load (it read the deleted internals), the journal's barrier at end_run admission (an acknowledged end could resume as a new run), the lane-error arms (a fatal attach transport rested ended_no_respawn; a terminal supervisor left its agent running) and the macOS hangup limit; the amendment 7 min ($1.19), 6b-0 ($8), then the relaunch 3.5 h wall in four agent rounds (the pass, three rounds after the gate and an outside review with four blockers, all fixed red first) and three landing fixes (the budget tests' backoffs behind a test seam; a macOS test's flag and its private run dir), about $31 for the agent; capsule 341/346/135/3 -> 273/315/130/3 (68k product and 31k tests deleted), rows 162/181/40/45 -> 163/183/42/45; the day went to 6b; 6c-1 was then checked against the code by a planner (11 min, about $2), which found it at about 540k counted whole, not the plan's share of 160k, and split it in four landings; 6c-1a1 (the row socket beside the old attach lane, additive) landed 19:23: 1.9 h wall (17:30 to 19:23) in two agent rounds (the pass 43 min to a gate-ready head, inside its clocks; one review round 15 min after an outside review with two blockers, a request queue a client could fill without bound and an eight-connection bound that freed slots before their threads ended, and seven fixes, all red first) and one landing fix by hand (the Windows pipe test opened a busy instance without retrying, as the product's client does), about 58 agent minutes; about $13 for the agent; capsule 273/315/130/3 -> 287/324/141/7 (twice up 11 while the old lane and the socket both stand; cross up 4 with the new edge to wire), wire 43/17/4/1 -> 43/18/4/1; the landing runs 7.8 min (Rust) and 3.9 min (CI); main's run after it found one more test race (a joined thread the kernel had not yet reaped, counted right after shutdown), fixed by reading the count until the reap, within 1 s |
| 7 | Frontend: State apart first, one short pass per cluster (the row record, focus and prompt, preview requests, the REPL and agent pane), each moving its fields into a type with private fields; then the five crate cuts (D4) as moves: app and render, chrome and input, navigation and session, agent pane and drawer, preview and control | 22-52 | 440-1,040 | |
| 8 | Sidecars (D3): 8a every child but the REPL, 8b the REPL and the pages channel | 4-8 | 80-160 | 8a: 2.6 h wall (08:49 to 11:25) in three agent rounds (the pass 28 min to a gate-ready head; one review round 20 min after an outside review that found two real defects, a workspace root interpolated into the kernel's Julia source and a line break splitting a Pluto command, both fixed red first, with the contract narrowed to the code; a 2-min tidy-up) and one landing fix (a test stand-in that read macOS's padded `wc` count), about 46 agent minutes; about $10 for the agent; sidecars 106/116/45/37 -> 106/120/45/37; twelve mechanism items queued; most of the wall was waits: a false local red from two gates sharing one build folder (queued GATE-TARGET-SHARED), and a hosted red in a capsule test race, fixed on main first by its own pass (c7c3d578b, 8 min, $0.33); three hosted runs, the landing one 6.9 min; 8b: 1.5 h wall (14:10 to 15:41) in two agent rounds (the pass 36 min to a gated head, with the port channel built and the REPL's guarantees settled; one review round 13 min after an outside review that found a blocked stdin write hiding a child's death and its grant, fixed red first, with two small fixes red first and the rest narrowed), about 66 agent minutes; about $9 for the agent; sidecars 106/120/45/37 -> 108/123/46/36, pages 36/48/11/27 -> 37/49/12/27; seven mechanism items queued; gates on the second gate host; two hosted rounds |
| 9 | Pages and files | 3-5 | 60-100 | |
| 10 | Agents: 10a the contract, the launch recipe, and the skills and work-state hooks only where a row is; 10b the launchers and the row CLIs | 2-4 | 40-80 | 10a: 2.3 h wall (15:48 to 18:06) in three agent rounds (the pass 20 min to a gate-ready head; one review round 13 min after an outside review that found four blockers, all fixed red first: a person's own skills removed by name at the first install, a linked skills folder followed, hooks removed by matching text, and sessions outside a row losing the mail check and the subagent guard; a 5-min round for the maintainer's decision that the four onboarding skills stay global) and two fixes by hand (a test's row id minted twice within one clock second, queued ROW-ID-MINT; two layer tests that asserted Unix paths on Windows), about 38 agent minutes; about $18 for the agent; agents 137/76/11/10 -> 139/78/11/10, distribution 221/254/60/23 -> 224/257/60/23; the landing runs 7.3 min (Rust) and 6 min (CI, its Julia package job re-run once after a package mirror stalled twice); a Windows red in the work-state tests was main's (a test fixture let its lock go before restoring the environment), found by a diagnosis, fixed on main (a5c724779); 10b (the launchers and the row CLIs) landed 19:57: 1.2 h wall (18:47 to 19:57) in two agent rounds (the pass 23 min to a gate-ready head; one review round 8 min after an outside review whose blocker was a guarantee promising more than agents owns: the work-state hooks and the Windows launcher's stop helper still read daemon files, now named as exceptions and queued with their owners; the rest narrowed or fixed red first; its first gate went red on one fixed sleep in a new test), about 31 agent minutes; about $12 for the agent; three tools that opened daemon files now ask the daemon; agents 139/78/11/10 -> 140/86/11/10; the landing runs 8.3 min (Rust) and 5.7 min (CI) |
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
  plan was checked against the code; its last closes the adopted-row limit rows' contract states. Pass 6c-1a1 landed
  2026-10-22: each row's supervisor serves the row socket beside the old lanes, and nothing uses it yet; 6c-1a2
  switches the callers. Nothing is half-done on main.
- Findings go to the owning subsystem's queue and are handled at that subsystem's pass.
