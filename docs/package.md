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

## Order of passes (the change each states, and the measured actuals)

Work runs continuously, paced by milestones: each pass starts when the previous one lands; the maintainer's iteration on a
contract gates its landing on main, not its start. A plan states the change: its rules, the files it touches, and its
product and test lines. It never states a duration, and no pass carries a session-hour or dollar estimate. The load
(every file the change reads or writes, whole and once, `wc -c` / 4, with the contracts it edits and those that cite
them) decides the context that holds the change whole, a 1M context when the subsystem is large; it never decides the
number of landings, and a change is split only when the change itself is large. A planner prompt asks for the change
and its traps, never a load split or a time. The standard: an edit past ten minutes is a flag, and a landing past thirty
minutes from its change to main is a failure, stopped and written up with its cause. Each new rule gets one test, red
before the change, and the proof is the build, the existing suite and those tests. Measured: the 37 window owners took 2
to 15 minutes of change each from one context holding the window; the Stop hook's card options took 9 minutes, the
direct page replies about 24. Each pass's measured history is in `docs/history/passes.md`, which no pass loads; the
estimates it carried are gone (2026-10-27, the maintainer: the fault in a three-landing, three-to-four-hour plan for
direct page replies, four rules in the page channel, was the estimate's).

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
Windows login and the other-account tests run at the release gate. The publish guard's Windows fix landed 2026-10-26: main's Rust run was red on Windows from the guard's speed landing (16:20) until this merge, unnoticed for about four hours because no one watched main's runs after that merge; the faster scanner counted a blob's lines against its size, and Git for Windows' awk drops a CR before a line's end, so every push carrying a CRLF blob was refused; a CR now reaches the matcher as another non-word byte, proven by the branch's full hosted run on all three systems. The installer's 0.7.0 minimum landed 2026-10-26, which closes the tag blocker INSTALL-WIN-UNBOUNDED-STEPS: the process listing and the Windows end-state `--version` asks are bounded at 60 s, and a listing with no answer is never "none runs"; tests that drive the install's phases with a cut listing wait for 0.7.1. The observation effort behind it is one miss: about four and a half hours of change across two agents and nine reviews (16:24 to 21:50), where the blocker needed about half an hour. Its class: each round bounded or read more of the machine than the blocker named, so each opened new answers to judge, first a new default at each call site, then hand-written second parsers of text our own subcommands print. The cure, our subcommands answering the installer as one JSON value of a shared type, waits on `pass/install-observe` for 0.7.1 with three named blockers. The batch's release-gate run 40448208255 on main at f771659ec is green whole: the native window jobs on Windows and macOS (the link's pane-timing route among them) and the Windows installer units and witness (64 minutes, the first real Windows installs with the bounded listing). The Stop hook's question line landed 2026-10-27 (9 minutes of change by a Codex edit job, main at 13:30): a SITREP-QUESTION line carries its options and recommendation, and a turn's first stop whose line lacks them is refused and stamps nothing, after the maintainer's Waiting on You page showed 8 of 10 cards without answer buttons. Direct page replies landed 2026-10-27: an opener grants one page entry a reply scope (`sot-fe docs --direct` a row's own page, `--direct-all` the two list pages, the open verb's page its own row; navigation and POST bodies never widen it); a scoped page's reply types at once or is held in memory until the row's prompt frees, a draft or a dialog refuses it, a row-scoped page answers only its own row, and the POST and `states.replies` answer sent, held, refused or unknown. Its change took about 24 minutes (the flag and scope edits 15 by a Codex job, the delivery 9 by hand) in one landing. It was first sized at three landings and three to four hours: the planner was asked for a load split, so it counted every contract whole as each landing's base (about 158k before code) and designed what the change did not need (a durable hold, question revisions, crash recovery); the change was four rules on the reply verb that 2a already had. A miss inside it: its first test run built no `sot-capsule` (a build line naming `--bins` twice), so the daemon suites ran twice. Nothing is half-done on main.
- Findings go to the owning subsystem's queue and are handled at that subsystem's pass.
