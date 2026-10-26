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
a stated gap); since 6d-1 a local connection is bounded, cancellable and reaches only this account (`transport::connect_own`
and the Unix socket and Windows pipe clients, moved from the capsule, with the Windows and macOS limits the contract
states), and a page secret is masked in the logs written through `RedactingWriter` (the daemon's and the window's).
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
sharing off (the shell bridge is a named exception); a login the window or the lane client starts is admitted through
the link gate, whose close ends the login a dial waits on (an established login ends when its stream closes); one
remote attach is one dial and one login (the spare login went in 6c-1a2); a lane dial is bounded and typed; endpoints are derived by one function each; a unit sotd writes starts only where hosts.toml gives
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
listing and `--version` asks stay unbounded until the observation pass, a 0.7.0 tag blocker. A push scans only commits the
destination does not hold: since 2026-10-25 the guard bounds its history scan by the push's old ids and by what the
destination advertises at push time (one `ls-remote` per push, read only when it would dial exactly the URL git hands the
hook; any error refuses), so a new branch or tag no longer rescans the history the server already has. Since 2026-10-25 no entry stands as a whole word anywhere in main's tree (three
replaced in four files).

**records.** Guarantees: the manual says what is true now; a design not built is marked unbuilt; no private names.
After the reorganization the package page and the contracts are the only descriptive records a session loads; ADRs
move to a history folder.

## Depends on

Today (crates): platform <- {containment, protocol, capsule, updater}; protocol <- capsule (the row socket's frames,
since 6c-1a1); {protocol, capsule} <- topology; the backend depends on
protocol, topology, updater, containment, capsule and platform, the frontend on all of those but containment. Wire
(protocol) depends on platform alone; topology on wire, platform and capsule (the capsule's `Endpoint`); containment
on platform alone. Inside `sot-backend`
(one binary, no library target) the modules form one cycle: {agents clients comm files lifecycle pages rows server
sidecars topology update}; inside the frontend, {lease net pages ui}; inside sot-capsule, {capsule endpoint
row supervisor} (since 6d-1; the local transport went to platform).

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
first, in a short pass of its own. A load counts every file the pass reads or writes, whole and once (`wc -c` / 4);
a scripted edit (a rename map) counts by its reviewed diff, and the pass opens such a file only at a compiler error,
its result file listing every file it opened. Beside its code a pass loads the package page's paragraphs it touches,
the contracts of the subsystems it edits and the contracts that cite them, found by search and named in its brief
(decided 2026-10-24: the package page and every contract measured about 102k at 61e5daaf2, not the 60k first
assumed). The planner check reads that whole base and names in the brief any other contract the pass must read; the
outside review reads whole. Passes near 180k are preferred, since those near 190k and above cost three to five times
more. Files the pass deletes without reading are not counted. A plan sizes its passes the same way: the capsule plan sized all of 6c at 160k, by
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
| CI | Distribution's CI: the hosted run at each landing about seven minutes and the local gate near five; the slow proofs to a release-gate run made once per release | 0.5-1 | 10-20 | eight short agent rounds, about 30 agent minutes in all; the rest was hosted runs (nine per-landing, five release-gate) and waits; about $13 for the agents; the per-landing run 48 -> 7.2 min wall measured alone (longest job 6.9 min), the release-gate run 21.5 min; the local gate 12 min, of which the bash 3.2 suites, which messaging deletes, are the wall; since 2026-10-22 the Julia package jobs install the preview tools (ghostscript, poppler-utils, ffmpeg) from a cache keyed by the runner image and the package list, after the Ubuntu mirror stalled five landing runs in one day: a hit installs in under 10 s and fetches nothing; a branch's cache is invisible to main, so main fills its own on its first run per runner image, which reads the mirror once, and on that miss a mirror that has not delivered within 140 s is replaced once by archive.ubuntu.com (proven by a forced run) |
| 5a | Platform and wire: their contracts; platform its own crate out of `sot-log`, its copies folded | 4-6 | 80-120 | 1.2 h wall in three agent rounds (the pass 25 min to a gate-ready head, the review round 6 min after an outside review, a landing fix 8 min), about 40 agent minutes; about $15 for the agent; platform 83/65/47/8 -> 79/80/48/0 (tests rose with the shared test helpers moved in; twice rose by one, generic names such as `context`), wire 42/14/3/2 -> 42/15/3/2; three hosted runs, the landing one 7.9 min |
| 5b-1 | Containment carved out of lifecycle into its own crate; lifecycle's contract (5b split by the load rule: 211k of code and tests) | 2-3 | 40-60 | 1.0 h wall in two agent rounds (the pass 23 min to a gate-ready head, the review round 5 min after an outside review), about 30 agent minutes; about $11 for the agent; lifecycle 42/67/39/15 -> 22/44/21/14, containment (new) 23/21/22/3; the landing run 12.2 min |
| 5b-2 | Topology: its contract; its own crate out of `rust/protocol`, which ends wire's edge to `sot-log` | 2-3 | 40-60 | 1.0 h wall in two agent rounds (the pass 21 min to a gate-ready head, the review round 5 min after an outside review), about 28 agent minutes; about $9 for the agent; seven defects fixed red first (unserialized `topology.set`, a sync that could overwrite a newer hub edit, three unchecked names reaching ssh or a socket path, a monitor label repeated across headers, a refused edit swallowing a hand edit); topology 50/52/35/26 -> 52/55/35/23, wire 42/15/3/2 -> 41/15/3/1; the landing run 6.9 min |
| 5c | Server: its contract; the op table into the binary, every service a handler with an explicit context | 4-6 | 80-120 | 0.9 h wall in two agent rounds (the pass 22 min to a gate-ready head, the review round 7 min after an outside review), about 29 agent minutes; about $7 for the agent; the op table was already one table and handlers already took explicit arguments, so the cut was the shared reply helpers and byte pump moved to wire, the server's submodules made private, pages and the updater given a closure instead of the roster; queue item 21 fixed on the real start path and the reaper fixed under event load, both red first; server 54/119/15/44 -> 52/118/14/45, wire 41/15/3/1 -> 43/17/4/1; the landing run 9.3 min |
| 6 | Capsule (D2), in passes from the plan: 6a voyage out, 6b one process per row, 6c-1 the row socket in four landings (a1 the socket served beside the old attach lane, a2 the window, wake, topology and bridge switched with the one protocol raise and the single dial, b the daemon's calls on the socket and the old lanes out, c rows on the socket in three: c1 rows read runs and the fence file, c2 the voyage folder out, c3 in two: c3a the watch served and the wake's debt per run, c3b the daemon on the watch, the adopted-row exit and run identity), 6c-2 in two landings (a the 0.7.0 retire of 0.6 rows and the launch's two dead flags, b the dormant drawer, rows' drawer arm, messaging's voyage handle form and the false lane prose), 6d the transport home, the capsule parser's dead flags and the contract | 10-20 | 200-400 | 6a: 1.4 h wall (07:43 to 09:10) in four agent rounds (the pass 34 min to a gate-ready head; one review round 8 min; two landing fixes, a test-only re-export the normal build could not see and two screen tests that read only the visible screen on Windows), about 45 agent minutes; about $35 for the agent; capsule 410/403/143/3 -> 341/345/135/3 (69k product and 58k tests deleted); the landing run 6.9 min; 6b stopped at its 45-minute line at 10:10 (49 min, 8 of the rows suite red: a plan gap at the rows boundary, not size), its capsule half kept on its branch; the amendment 7 min, $1.19; 6b-0 (the rows suite reads only the surface that survives 6b, no product code) 2.3 h wall (10:27 to 12:47) in two agent rounds (the pass 16 min to a gate-ready head; one review round 18 min after an outside review that found four tests that would still break on 6b and a pid-reuse race in the new kills, which now go through a pidfd), about 38 agent minutes; about $8 for the agent; rows 162/182/40/45 -> 162/181/40/45, capsule 341/345 -> 341/346; the wall was mostly hosted runs queued behind two fixes on main; the landing run 9.0 min; 6b (one process per row) landed 17:11: the first run stopped at its 45-minute line (50 min, about $14) on five design gaps found after the plan, an 18-minute read: the watchdog's restart budget (the deleted anti-flap counter had been what turned an unlaunchable agent terminal), the rows integration suite left out of the load (it read the deleted internals), the journal's barrier at end_run admission (an acknowledged end could resume as a new run), the lane-error arms (a fatal attach transport rested ended_no_respawn; a terminal supervisor left its agent running) and the macOS hangup limit; the amendment 7 min ($1.19), 6b-0 ($8), then the relaunch 3.5 h wall in four agent rounds (the pass, three rounds after the gate and an outside review with four blockers, all fixed red first) and three landing fixes (the budget tests' backoffs behind a test seam; a macOS test's flag and its private run dir), about $31 for the agent; capsule 341/346/135/3 -> 273/315/130/3 (68k product and 31k tests deleted), rows 162/181/40/45 -> 163/183/42/45; the day went to 6b; 6c-1 was then checked against the code by a planner (11 min, about $2), which found it at about 540k counted whole, not the plan's share of 160k, and split it in four landings; 6c-1a1 (the row socket beside the old attach lane, additive) landed 19:23: 1.9 h wall (17:30 to 19:23) in two agent rounds (the pass 43 min to a gate-ready head, inside its clocks; one review round 15 min after an outside review with two blockers, a request queue a client could fill without bound and an eight-connection bound that freed slots before their threads ended, and seven fixes, all red first) and one landing fix by hand (the Windows pipe test opened a busy instance without retrying, as the product's client does), about 58 agent minutes; about $13 for the agent; capsule 273/315/130/3 -> 287/324/141/7 (twice up 11 while the old lane and the socket both stand; cross up 4 with the new edge to wire), wire 43/17/4/1 -> 43/18/4/1; the landing runs 7.8 min (Rust) and 3.9 min (CI); main's run after it found one more test race (a joined thread the kernel had not yet reaped, counted right after shutdown), fixed by reading the count until the reap, within 1 s; 6c-1a2 (the switch: every viewer on the row socket, protocol 4, one login per remote attach) landed 22:02: 2.6 h wall (19:25 to 22:02) in two agent rounds (the pass 74 min, 29 past its clock, with three local gates of its own; one review round 32 min after an outside review with two blockers, a terminal reply still written on the run's loop and queued input that could reach a replacement run, all fixed red first; two decisions taken there with the captain: a slow viewer is resynced with a fresh screen, never cut, and a row socket that cannot be bound exits 75 into rows' budget) and one landing fix found by a diagnosis (19 min, about $2.50): under a flood every resync's screen was kept as an answer, 64 of them cut the window, and the first Ctrl-C never reached the agent, which the test's retyped Ctrl-C had hidden; about 106 agent minutes, an hour of it waiting in the foreground on test runs; about $52 for the agent; capsule 287/324/141/7 -> 251/288/145/7, topology 52/55/36/23 -> 51/51/34/23, rows 163/184 -> 162/183; the old attach lane's server is left for its own landing; 6c-1a3 (the old attach lane out) landed 23:11: 1.0 h wall (22:07 to 23:11) in two agent rounds (the pass 31 min to a gate-ready head, inside its clocks; one review round 7 min after an outside review with no blocker: a row listener that died left the agent unreachable, an end left queued input to time out, and a Windows fidelity test deleted without its replacement, all fixed red first), about 38 agent minutes; about $14 for the agent; 61 files, 11k lines deleted; capsule 251/288/145/7 -> 198/225/141/7; 6c-1b was then checked against the code by a planner (11 min) and split in two (one pass loaded about 221k), and it found a trap the old plan missed (rows' phase cell orders on the voyage, so feeding it the run would latch a resting row after a reset; the voyage stays in status until 6c-1c); 6c-1b1 (management on the row socket) landed 01:13: 1.6 h wall (23:27 to 01:13) in two agent rounds (the pass 56 min, 11 past its clock: the brief named one test that read the old leg number and there were thirteen, plus four direct end calls that now pass the run; one review round 12 min after an outside review with two blockers, a Windows stop a silent peer could hang and a listener lost while the row rests that stranded the supervisor holding its fence, and the brief's own decision that a lost reply reads status, never built; all fixed red first), about 68 agent minutes, 45 of them waiting on test runs; about $17 for the agent and $2.60 for the planner; capsule 198/225/141/7 -> 189/227/142/7; 6c-1b2 (the lane remnants out) landed 02:13: 1.0 h wall (01:14 to 02:13) in two agent rounds (the pass 21 min, inside its clocks; one review round 3 min after an outside review with no blocker, which asked for a cancellation test that cannot pass through the row's own timeout, the malformed-wire exchange tests ported to the row's hello, and records that named deleted files) and one gate re-run (the agent built test programs in its gate's target folder while the gate ran, relinking the daemon under a comm test: GATE-TARGET-SHARED again) and one landing fix by hand (a join-deadline test it had moved from Windows to every system needed a 100 ms sleep inside 150 ms and missed on macOS; it now lets the shared deadline pass and requires an immediate refusal), about 24 agent minutes; about $15 for the agent; capsule 189/227/142/7 -> 118/140/99/7, the night's largest cut; six tests of platform's isolated-test harness went with the socket suite and are queued to platform; 6c-1c was then checked against the code by a planner (5 min, about $1), which found it at about 250k and split it in three, and found this series' brief gaps: the reauth restart file that reads the voyage was missing from the plan, and 31 test files read the voyage, not ten; 6c-1c1 (rows read runs and the fence file) landed 03:51: 1.5 h wall (02:21 to 03:51) in three agent rounds (the pass 42 min to a gate-ready head, inside its clocks, with the first-start restart defect closed by the fence read alone; one review round 4 min after an outside review with two blockers, an absence check that created the start marker so a never-started row ended at a close would resume at the next boot, and a lost pointer that resurrected a requested end, both fixed red first; a 6-min follow-up after the architect found the second fix still resurrected the end at a second restart) and one landing fix by hand (two sign-in poll tests asserted exact counts a whole-second clock cannot promise and went red once each under the gate's load), about 53 agent minutes, 36 of them waiting on test runs; about $13 for the agent; rows 162/183/43/46 -> 162/183/43/44, capsule 118/140/99/7 -> 118/142/99/7; the branch's hosted runs 7.9 and 8.7 min; 6c-1c2 was checked against the code again by a planner (14 min, about $4), which found five traps in its old section, the record of why the check earns its ten minutes: deleting the voyage folder's mkdir would have broken every Windows start (the only thing that made the state folder there), the run's lock was the volume proof's caller, an answered reset could be undone by a kill before the next run, and two tests rode the old folder; 6c-1c2 (the voyage folder out) landed 05:33: 1.4 h wall (04:10 to 05:33) in two agent rounds (the pass 49 min to a gate-ready head, 4 past its clock; one review round 8 min after an outside review with three blockers, all fixed red first: an end admitted after the agent ended by itself was answered `ended` unrecorded, so a restart ran the agent again; a reset whose record write failed ran on into a new run; a reset published by a deletion Windows cannot flush, now by a durable write), about 58 agent minutes, 30 of them waiting on test runs; about $16 for the agent; capsule 118/142/99/7 -> 116/141/96/7, rows 162/183 -> 162/182; two hosted reds were the runners' (a Windows shard cancelled at its bound after a 4-min Julia unpack, a macOS name lookup failure), the second re-run green; 6c-1c3 was checked again by a planner (20 min, about $6), which found it at about 195k and split it in two, corrected two premises (a run's identity cleared at the watch's end would count every exit never-ready; the observer is not the cell's one writer) and closed eight traps (among them an exit frame the server's own shutdown dropped, and an announcement under the supervisor's lock that deadlocked); 6c-1c3a (the watch served, the wake's Enter per run) landed 09:07: 1.0 h wall (08:08 to 09:07) in two agent rounds (the pass 21 min to a gate-ready head; one review round 10 min after an outside review with three blockers, all fixed red first except one regression test the old code also passed: each change was announced by rereading the state, so a quick end lost its `ending` report, now recorded under the lock and told in order; a wake's Enter could land in a run that replaced the one it checked, now input names its run and the row refuses another; a watch admitted during the exit got no exit code) and one hosted red outside the pass (a platform test bounded a panic's settling at 5 s and a Windows runner's backtrace print took 9.7 s; its bound is now 20 s against a 60 s deadline), about 32 agent minutes, 15 of them waiting on test runs; about $8 for the agent; capsule 116/141/96/7 -> 120/147/100/7, messaging 95/125 -> 95/126; the branch's hosted run 8.4 min; the row then moved to a second account at the first's weekly cap, on the maintainer's direction; 6c-1c3b was checked again by a planner (16 min, about $5), which closed three traps the first half made (a restart that raced the exiting supervisor's fence, a Windows row read stopped while its supervisor ran, a Windows exit 69 read stopped); 6c-1c3b (the daemon on the watch) landed 11:58: 1.4 h wall (10:35 to 11:58) in two agent rounds (the pass 30 min to a gate-ready head; one review round 15 min after an outside review with two blockers sharing one cause, an exit not bound to the supervisor that made it, so an old exit 69 could latch a healthy replacement terminal and an exit already restarted could be restarted again, now every outcome carries the row's spawn generation and is acted on only at it), and three fixes outside the pass (a wake test counted both daemons' boot looks, which the 2 s ready made two; a containment test read a dead process's pid reused by a thread as an error; the wake shard ran past its 5-minute bound with every test passing and three tests moved to wake3); the red commits were not run, their tests red by reading and compilation; about 48 agent minutes, 20 of them waiting on test runs; about $15 for the agent; rows 162/182/43/44 -> 164/187/43/44, capsule 120/147 -> 120/148; the branch's hosted run 7.6 min; 6c-1 is complete; 6c-2 was checked against the code by a planner (32 min, about $14), which found it at about 350k and split it in two, and found five traps: a 0.6 stop ends only the supervisor, so a retire that only stopped it would start a second agent beside the old one, both holding the row's mail handle; 0.6.6 records a person's end in its journal, not in an `ended` file; the start sweep never looks inside a registered row's folder; the old supervisor journals the retire's own end; a Cleanup boot ends rows without resuming them; the captain took the cut line at once (the parser's dead flags to 6d, about 228k to 184k) and had boot retire every 0.6 row at once rather than four at a time, so twenty rows cost the slowest one; 6c-2a (the 0.6 rows retired) landed 07:59: 1.2 h wall (06:49 to 07:59) in two agent rounds (the pass 35 min to a gate-ready head, inside its clocks; one review round 8 min after an outside review with three blockers: a retry after a daemon's death skipped the row's Linux scopes, a person's end seen only on the live old supervisor was lost if the daemon died after its stop, and a macOS kill by process number, platform's stated residual, now named in rows' contract; and five more: malformed journal entries read as no end, the retire's own end read as the person's on a retry, the day-later sweep racing a retire, a start marker the end path could create, and tests at those boundaries), about 43 agent minutes; about $14 for the agent; rows 164/187/43/44 -> 174/198/57/45 (twice up 14 with the retire's frame and lane names, deleted with it after 0.7.0), capsule 120/148/100/7 -> 120/148/101/7; the branch's hosted runs 7.75 and 7.5 min; 6c-2b (the voyage era's last arms out: the window's dormant attach-only drawer, rows' drawer arm and messaging's voyage handle form) landed 08:31: 0.5 h wall (08:01 to 08:31) in two agent rounds (the pass 9 min to a gate-ready head, both red tests red by behavior; one round of a minute after an outside review with no blocker, whose findings were a contract sentence that promised more than the comm commands do, since a 0.6 session's send and poll fail with exit 1 and its identity lookup may still answer its own old name, and leftover drawer comments), about 11 agent minutes; about $3 for the agent; 361 lines deleted and 78 added; rows 174/198/57/45 -> 173/199/57/45, fe-ui 390/183/69/30 -> 388/183/69/29, messaging 95/126 -> 95/127; the branch's hosted runs 7.5 and 8.3 min; 6c-2 is complete; 6d was checked against the code by a planner (29 min, about $10), which found the crate rename alone writes 204 files, about 860k by the load rule, not the plan's 110k, and split 6d in three landings (the homes, the name, the contract); the captain took its decisions: a scripted rename counts by its reviewed diff, a pass loads only the contracts it touches (the full base measured about 102k, not 60k), and one build-only release-workflow run proves the rename before a tag; 6d-1 (the homes: the local connection client, the shared test support and the log masking to platform; the capsule's two dead flags out) landed 10:08: 1.1 h wall (09:03 to 10:08) in two agent rounds (the pass 33 min to a gate-ready head; one round of 3 min after an outside review whose blocker was the source-text half of the new dependency test, against the rule that no test proves code by scanning its text: it went, and the dependency graph read through cargo stays as a guard; three of platform's new promises were narrowed to what holds on Windows and macOS) and one landing fix by hand (the hosted run's catch-all test group excludes platform's package and could not build the three tests that moved there; they are named in platform's group, queued SHARDS-REST-EXCLUDE), about 36 agent minutes; about $13 for the agent; platform 80/79/52/0 -> 100/89/54/0, capsule 120/148/101/7 -> 98/138/96/9; the branch's hosted run 9.4 min; 6d-2 (the name: the crate `sot-log` is `sot-capsule` in `rust/capsule/`, the fence module out of the old journal, the build stamp renamed, and the throttled relay's half-close fixed, queue item LANE-BRIDGE-RELAY-HALF-CLOSE) landed 11:32: 1.4 h wall (10:10 to 11:32) in two agent rounds (the pass 35 min to a gate-ready head, the rename a scripted map whose diff was reviewed, 230 files, the lock file changing names only; one round of about 10 min after an outside review that found no stale name in the workflows, shards, release and pack scripts, gates or installer, and asked for two guards that proved less than their names and records the map missed), about 45 agent minutes; about $11 for the agent; the release workflow ran once on the branch, build-only (its draft job skipped), with all 22 jobs green, the only proof of the renamed release build before a tag; capsule 98/139/96/9 -> 98/141/96/9, everything else unchanged; the branch's hosted runs 8.0 and 8.9 min; 6d-3 (the capsule's contract, 13 guarantees each with its test) landed 15:09: 0.9 h wall (14:13 to 15:09) in two agent rounds (the pass 14 min to a gate-ready head, with four new guards each shown red against its code; one round, the first run as a Codex job on the maintainer's word that agent work moves to Codex, 8 min, after an outside review whose findings were promises stated beyond the code: degraded starts, a Windows writer with no five-second cut, the alternate screen, timing totals that were component budgets, a derived ready an end can pass; the Codex job's sandbox could not reach the build host or finish its last commit, so the architect committed and gated), a hosted Windows red that was the new guard discarding saved history, fixed in the test; about $7 for the Claude agent; capsule 98/141/96/9 -> 98/144/96/9; the branch's hosted runs 7.5 and 7.4 min; the capsule series (D2) is complete |
| 7 | Frontend: State apart first, one short pass per cluster (the row record, focus and prompt, preview requests, the REPL and agent pane), each moving its fields into a type with private fields; then the five crate cuts (D4) as moves: app and render, chrome and input, navigation and session, agent pane and drawer, preview and control | 22-52 | 440-1,040 | planned 2026-10-25 by a Codex planner (18 min) against the code: State has 236 fields extended in 75 files, so 39 landings (31 owner passes, two preparations for an fe-net crate and the lease client, six crate moves, fe-net among them), 31 to 52 session hours, each pass near 180k; the plan is `dev/output/reorg/passes/7-frontend-plan.md` (local); 7a (`RowDirectory`: the row listings and canonical-id translation, private) landed 03:45: about 1.1 h wall (02:44 to 03:45), every agent on Codex (the implementer 16 min, a test compile fix, one review round 8 min after an outside review that said LAND with three notes: a kept-default control weaker than the charter, translation over every listed host named and pinned, and a helper queued to the result-ledger pass); the five owner tests each shown red by a deliberate wrong owner and green restored; fe-ui 388/183/69/29 -> 389/185/70/29; the branch's hosted run 8 min; 7b (`RowPresentation`: each row's label, root, work state, previous state, flash start, strip order and default slug, private, returning effects; the REPL's lifecycle and the declared-session memory left for their own passes, which would have taken it to about 195k) landed 05:43: about 1.7 h wall (04:00 to 05:43), a Codex planner check (15 min) wrote its brief at 184.8k, then the implementer 17 min, one review round 9 min after an outside review that found two tests weakened in the move (rank before time across tiers, cycling direction) and a flash start sampled once before projection, all restored; the 22 owner tests each shown red by a deliberate wrong owner Codex wrote and green restored; fe-ui 389/185/70/29 -> 387/187/76/29 (twice up 6: three `State` wrappers that keep the owner's method names until the application pass, and generic names such as `refresh`); the branch's hosted runs 9 min (Rust) and 12.5 min (CI); 7c (`ResultLedger`: the pending results, their result and attempt serials and the reveal, private, returning effects; the helper that cloned every pending id deleted, queued by 7a's review) landed 06:11: about 1.1 h wall (05:08 to 06:11), a Codex planner check (14 min, 184.8k), the implementer 18 min, which counted its own load at about 200.6k, over the 185k line, from broad searches that opened files beyond the manifest (the next briefs name the whole read set); one review round 4 min after an outside review that found two transition checks masked by the next step and a tuple to delete; the eleven ledger tests each shown red by a deliberate wrong owner and green restored; fe-ui 387/187/76/29 -> 388/192/78/29; the branch's hosted runs 11 min (Rust) and 19 min (CI); 7d (`AgentPane`: the agent pane's target row and attachment, terminal size, selection and drag, attach episode with its clocks and warnings, warm clients, held screen, feed, dial errors and discarded input, private, returning effects) landed 07:28: about 1.5 h wall (05:57 to 07:28), a Codex planner check (16 min, 183.2k), the implementer 35 min (about 180k by its count), a test expectation fixed (the held screen keeps its text; only the clipboard trims), one review round 12 min after an outside review that found an eviction test weakened, wheel presence left only to a release-gate fixture (now also a unit test through a narrow context port, so it runs each landing) and the attach log moved after the client stamp (restored), and a focus scan's false positive on the new test fixed in the test; the 20 owner tests each shown red by a deliberate wrong owner and green restored; fe-ui 388/192/78/29 -> 386/198/91/29 (twice up 13: the pane owner's methods keep the names of the `State` wrappers that call them, until the application pass); the branch's hosted runs 11 min (Rust) and 3 min (CI). Native evidence: the result fixtures and pane timing run only in release-gate.yml, never in a landing run, so 7a to 7d landed with it owed; it is in release-gate run 40197701006 on main at 711d83d4f (native minimized window with the result fixtures, pane timing and window close, green on Windows and macOS); from then on one run after every third pass 7 landing and before any landing on unproven pane, result or rendering owners (the captain, 2026-10-25), each landing naming its run; 7e (`ReplModel`: the REPL's rows, eval routing, log, input, run-file status, interrupt, scroll, pkg mode, history and the lifecycle 7b left, private, returning effects; its presentation left for 7f) landed 08:02: about 1.4 h wall (06:41 to 08:02), a Codex planner check (16 min, 184.2k), the implementer 18 min (184.1k by its count), one review round 6 min after an outside review that found a delayed demo eval resetting the scroll, run announcements tested around their frame, and diagnostics lost in the move, all restored; the 20 owner tests each shown red by a deliberate wrong owner and green restored; fe-ui 386/198/91/29 -> 384/205/94/29; the branch's hosted runs 9 min (Rust) and 4 min (CI); native evidence in release-gate run 40204152703 (on 7f's branch, carrying 7e); 7f (`ReplPresentation`: the REPL's images, image slots, scrollback pixels, visible window and build anchor, private, returning effects; one named defect fixed, P7f-image-alias: two rows with equal eval and frame ids no longer paint each other's image) landed 09:07: about 1.6 h wall (07:28 to 09:07), a Codex planner check (13 min, 180.45k, found the plan's 120-150k reservation too small), the implementer 30 min, a fixture fix (a wire field misspelled in a test), one review round 6 min after an outside review that asked for its own native evidence and for two image-cache rules a test could tell apart; the 30 tests each shown red by a deliberate wrong owner and green restored; fe-ui 384/205/94/29 -> 384/211/100/29; the branch's hosted runs 9 min (Rust) and 10 min (CI); native evidence in release-gate run 40204152703 (minimized window, pane timing and window close green on Windows and macOS); 7g (`FocusPrompts`: focus, the nav prompt, scale entry's prior focus, relaunch and the quit hint, private; the two focus source scans replaced by behavior tests and their file deleted; focus changes still leave the quit dialog open) landed 11:10: about 2.6 h wall (08:33 to 11:10; it waited behind the publish guard's fix), a Codex planner check (13 min, 180.4k), the implementer 24 min (185.0k by its count), one review round 4 min after an outside review that found two log events moved to a new target, the hint's clock sampled before its text, a test bypassing the owner's admission and a lost backslash case, all restored, and three dead pieces deleted; the 21 tests each shown red by a deliberate wrong owner and green restored; fe-ui 384/211/100/29 -> 384/215/105/29; the branch's hosted runs 17 min (Rust) and 7 min (CI); native evidence in release-gate run 40219701337 on main at 9cba599d1 (minimized window, pane timing and window close green on Windows and macOS); 7h (`LayoutHelp`: the drawer's tenant, maximize, wide preview, Help with its origin and peek, and the two pending Help starts, private, returning effects) landed 15:12: about 3.4 h wall (11:48 to 15:12), a Codex planner check (17 min, 183.9k), the implementer 56 min, three fix rounds of a few minutes (a moved log line that tracing 0.1.44 cannot parse with a dotted field after a target, now in its braced form; two tests that probed the peek's fade a nanosecond before its end, where the f32 opacity is already zero, as before the move), one review round 5 min after an outside review that found a vacuous selection reset, pane widths left unasserted and three always-empty effects (deleted), and asked for the pass's own native evidence; the 22 tests each shown red by a deliberate wrong owner and green restored; fe-ui 384/215/105/29 -> 386/223/121/29 (twice up 16: four effect ports counted at their traits and at `State`'s impls, owner methods that keep the `State` wrappers' names, generic names such as `query`); the branch's hosted runs 10 min (Rust) and 3 min (CI); native evidence in release-gate run 40227928042 on the branch at cf72d3472 (minimized window, pane timing and window close green on Windows and macOS) |
| 8 | Sidecars (D3): 8a every child but the REPL, 8b the REPL and the pages channel | 4-8 | 80-160 | 8a: 2.6 h wall (08:49 to 11:25) in three agent rounds (the pass 28 min to a gate-ready head; one review round 20 min after an outside review that found two real defects, a workspace root interpolated into the kernel's Julia source and a line break splitting a Pluto command, both fixed red first, with the contract narrowed to the code; a 2-min tidy-up) and one landing fix (a test stand-in that read macOS's padded `wc` count), about 46 agent minutes; about $10 for the agent; sidecars 106/116/45/37 -> 106/120/45/37; twelve mechanism items queued; most of the wall was waits: a false local red from two gates sharing one build folder (queued GATE-TARGET-SHARED), and a hosted red in a capsule test race, fixed on main first by its own pass (c7c3d578b, 8 min, $0.33); three hosted runs, the landing one 6.9 min; 8b: 1.5 h wall (14:10 to 15:41) in two agent rounds (the pass 36 min to a gated head, with the port channel built and the REPL's guarantees settled; one review round 13 min after an outside review that found a blocked stdin write hiding a child's death and its grant, fixed red first, with two small fixes red first and the rest narrowed), about 66 agent minutes; about $9 for the agent; sidecars 106/120/45/37 -> 108/123/46/36, pages 36/48/11/27 -> 37/49/12/27; seven mechanism items queued; gates on the second gate host; two hosted rounds |
| 9 | Pages and files | 3-5 | 60-100 | |
| 10 | Agents: 10a the contract, the launch recipe, and the skills and work-state hooks only where a row is; 10b the launchers and the row CLIs | 2-4 | 40-80 | 10a: 2.3 h wall (15:48 to 18:06) in three agent rounds (the pass 20 min to a gate-ready head; one review round 13 min after an outside review that found four blockers, all fixed red first: a person's own skills removed by name at the first install, a linked skills folder followed, hooks removed by matching text, and sessions outside a row losing the mail check and the subagent guard; a 5-min round for the maintainer's decision that the four onboarding skills stay global) and two fixes by hand (a test's row id minted twice within one clock second, queued ROW-ID-MINT; two layer tests that asserted Unix paths on Windows), about 38 agent minutes; about $18 for the agent; agents 137/76/11/10 -> 139/78/11/10, distribution 221/254/60/23 -> 224/257/60/23; the landing runs 7.3 min (Rust) and 6 min (CI, its Julia package job re-run once after a package mirror stalled twice); a Windows red in the work-state tests was main's (a test fixture let its lock go before restoring the environment), found by a diagnosis, fixed on main (a5c724779); 10b (the launchers and the row CLIs) landed 19:56: 1.2 h wall (18:47 to 19:56) in two agent rounds (the pass 23 min to a gate-ready head; one review round 8 min after an outside review whose blocker was a guarantee promising more than agents owns: the work-state hooks and the Windows launcher's stop helper still read daemon files, now named as exceptions and queued with their owners; the rest narrowed or fixed red first; its first gate went red on one fixed sleep in a new test), about 31 agent minutes; about $12 for the agent; three tools that opened daemon files now ask the daemon; agents 139/78/11/10 -> 140/86/11/10; the landing runs 8.3 min (Rust) and 5.7 min (CI) |
| PG | Distribution: the private-name publish guard moves into the repository and the side repository retires; A the guard and clone enrollment, B the installer | 1-2 | 10-20 | A landed 23:32: 3.0 h wall (20:30 to 23:32), every agent on Codex (the maintainer's order): the planner check 24 min, which found 236k and split it, and found that the installer already ran `git worktree prune`, removed as its own commit; the implementer 30 min, then two test fixes, one review round (four blockers: a library that could not load let a push pass, inherited git selectors could enroll another clone, a UTF-16 list matched nothing, a vanished list could print its path) and four Windows rounds, all test assumptions of POSIX on Git Bash (path forms, permissions) fixed as one class or skipped there with the limit stated; a check against the real list by counts only caught that an ASCII-only rule would have refused it (its comments hold UTF-8), so the rule is UTF-8 without a byte-order mark; the CI job runs one suite per job, since Windows Git Bash took past ten minutes for all three; distribution 224/257 -> 227/275; B landed 07:07: 6.5 h wall (00:37 to 07:07), every agent on Codex: the implementer, three Windows rounds (a shared fallback pipe from a missing USERNAME, native paths, PowerShell's environment), a planner core review after the third, one review round after an outside review (the guard's children unbounded, the verdict silent on the guard), then landing fixes from the hosted runs: the first 30 s bound cut real Windows enrollments (now 60 s a Git read, 300 s the setup script), a diagnostic matrix showed the installer's process query takes about 26 s cold on a hosted Windows runner in any environment (speed, not the isolated profile), and the nine real-enrollment units and the install witness took minutes each there, so on the captain's word they run on Windows once per release, proven green before landing in release-gate run 40189400665 (units 31 min, witness 46.5 min); load 237k by the rule, 287k with the three hosted logs the Windows rounds read, over the 180k target and, with the logs, the 250k ceiling; queued: the installer-observation pass (a 0.7.0 tag blocker), one Windows shell-test harness, the setup script's Windows speed; distribution 227/275/58/23 -> 233/291/63/24 |
| 11 | Release gate: the full hosted matrix once, install on every box, the maintainer's use, the tag (0.7.0); the installers' remaining proof is here: an rc9.15 box's old prelude handing off to the first candidate on a backend-only host with glibc 2.31, and the maintainer's use on the maintainer's own boxes | 3-6 | 60-120 | |

Total: about 78 to 157 session hours, 1,560 to 3,140 dollars, most of the range in the frontend and the capsule.
Wall time is longer by the hosted runs and the maintainer's iterations; a second pass in parallel on a disjoint subsystem (in a
worktree, merged when its contract settles) shortens the calendar, not the work.

## Integration tests: which pass reduces each

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
needed its own build folder and node packages). Nothing is half-done on main.
- Findings go to the owning subsystem's queue and are handled at that subsystem's pass.
