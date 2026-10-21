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
The design records (52 ADRs) and the old folder pages live in `docs/history/` and load nowhere.

## Draft contracts, one paragraph each

**platform.** Guarantees: `host_name` errs, never guesses; a publish is no-clobber and fsynced to the parent; a kernel
lock dies with its holder and no exec'd child holds it; a descriptor the lanes or the capsule make is born
close-on-exec (macOS under a birth lock); a local reply is trusted only after the challenge; storage exhaustion is
known by native code only. Rests on: the OS (renameat2/renamex_np/MoveFileEx, flock/LockFileEx, O_CLOEXEC). Owns no
runtime state. Does not: start processes for anyone but the native launcher it hands out.

**wire.** Guarantees: an envelope is at most 1 MiB in every reader and writer; a blob is exactly its declared length;
a payload grows only by defaulted fields, else the protocol integer rises and hello refuses the mismatch; a child's
output is read by lines lossily, so no byte ends a reader. Rests on: serde and the compiler. Owns: nothing at run
time. Does not: decide policy from the version string (only `is_release_build`).

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

**rows.** Guarantees: a slug keeps its id across restarts; one lifecycle guard per row; a launch takes the row's fence
before it forks, so no second supervisor is born over a live one; no two rows hold one declared handle; the phase cell
changes only through observation of the current supervisor epoch. Rests on: capsule (supervisor, attach), agents
(recipe), containment, platform (fence). Owns: the registry, row tomls, state folders. Does not: run the agent's
terminal (capsule) or read its screen for mail (messaging's wake).

**capsule.** To the rest: a row's terminal survives the window and the daemon; one supervisor per state dir, so rows
never start a second; a viewer (window, headless client, wake) attaches through one client and sees the screen as it
is; rows read the supervisor's end as clean, terminal, contended or storage-held. Inside (current mechanism): an
append-only voyage read forever, codec seams, torn-tail recovery, fsync before publish, ground-gated attach, three
lanes, exit codes 0, 69, 70, 71. Rests on: platform (locks, durable publish, process birth), the OS pty/ConPTY. Does
not: know about rows, mail or windows. Decision D2 decides how much of the inside stays.

**messaging.** Guarantees: `filed -> @h` only on the appender's word for a live handle, `FAILED` appended nothing;
every append under the inbox lock; only poll moves a cursor; every registry write under its lock; a wake line is typed
only into a free prompt of the receiver's row. Rests on: platform locks, rows (the row's terminal), topology (the hub
link). Owns: `~/.sot-comm` data. Does not: work-state (no 0031 guarantee; leaves for rows' display). Decision D1.

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

**distribution.** The release path's contract is settled (pass 1a): `scripts/CLAUDE.md`, "Release path". The cut
tags only on the maintainer's word and runs no tests and reads no hosted run; the local gate is `rc-gate.sh` plus
`user-manager-tests.sh`; CI only drafts. The installers' half, pass 1b's: install verifies checksums before any
write and writes `install.json` last; a refused install writes nothing; an install never moves backwards; apply
verifies before it mutates, restores on failure and exits 0; the launch path never stops on an update; every exit of
an installer ends in one verdict line. Rests on: the release tag, `sotd` subcommands (`update direction`,
`same-version`, `topology plan`), GitHub releases. Owns: `$PREFIX`, the units, the comm install. Decision D5.

**records.** Guarantees: the manual says what is true now; a design not built is marked unbuilt; no private names.
After the reorganization the package page and the contracts are the only descriptive records a session loads; ADRs
move to a history folder.

## Depends on

Today (crates): log <- protocol <- {backend, frontend}; log <- updater <- {backend, frontend}. Inside `sot-backend`
(one binary, no library target) the modules form one cycle: {agents clients comm files lifecycle pages rows server
sidecars topology update}; inside the frontend, {lease net pages ui}; inside sot-log, {attach_client capsule host
identity lane store supervisor}.

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
  agents pass. Live rows end once at the 0.7.0 install.
- **D3 sidecars.** The kernel, the REPL, Pluto and the monitor sampler stay; the Linux per-child X display stays (a
  security boundary). The dormant Claude SDK producer (`rust/log/src/claude.rs`, `claude-sdk-helper/`, 47k tokens,
  no product path runs it) is deleted in the sidecars pass. MathJax's node child is decided in that pass on one fact:
  whether markdown with math is previewed in use.
- **D4 the frontend splits into five crates** along the existing event router (`drain_events`, one arm per event):
  app and render; chrome, input, help and persistence; navigation and session; agent pane and drawer (terminal, REPL,
  monitor); preview (image, markdown, editor, concept) with the control surface. Held to the cross-crate call count
  (the fourth number). The first frontend pass is the split itself.
- **D5 the installer's logic moves into Rust.** The rules written twice today (3.8k lines of sh, 5.6k of PowerShell,
  three byte-equal copies of two helpers) move into `sotd install`, which the pack already carries; `install.sh` and
  `install.ps1` shrink to fetch, verify and exec it. Distribution goes in two passes: 1a the release path first, with
  the thirty-minute done test and the outside review, then 1b the installer.
- The repo's committed permission settings (`.claude/settings.json`, step 0) allow its own build, check, test, lint,
  gate, pack, workflow-run and install commands and reading its releases; nothing in them tags or releases.

## Order of passes (session hours and dollars, estimates until each pass is measured)

Work runs continuously, paced by milestones: each pass starts when the previous one lands; the maintainer's iteration on a
contract gates its landing on main, not its start. Rate assumed: about 20 dollars a session hour at high effort (the
package page: 15 minutes, 5 dollars). Each estimate is replaced by the measured figure when its pass lands.

| # | pass | hours | dollars | measured |
|---:|---|---:|---:|---|
| 0 | Mechanical preparation: tests out of product files into sibling files; the 117 folder pages (164k tokens) to one-line pointers, their full text kept in `docs/history/`; the 52 ADRs (215k tokens) to `docs/history/adr/`, loaded by nobody; the root CLAUDE.md to this page; the size-limit and page checks retired; memory pruned to operating state; the four-number script | 3-6 | 60-120 | 0.7 h, about 30-35 dollars; product 1,959 -> 1,962k, tests 2,087 -> 2,034k |
| 1a | Distribution, the release path: `release.sh` without its test re-run or green-at-HEAD rule, one hosted run, the pack; done test: thirty minutes from fixes in hand to a candidate installed on the maintainer's workstation; the outside review | 2-4 | 40-80 | 1.6 h (pass 1.1, gate and landing 0.5); done test 22.4 min by segments; distribution 215/295 -> 213/298k |
| 1b | Distribution, install, update and launch (D5) | 6-12 | 120-240 | |
| 3 | Rows, with work-state and how a row's state is shown | 4-8 | 80-160 | |
| 4 | Messaging (D1) | 6-12 | 120-240 | |
| 5 | Daemon core: server, lifecycle and containment carved; wire, topology and platform contracts | 6-10 | 120-200 | |
| 6 | Capsule (D2) | 10-20 | 200-400 | |
| 7 | Frontend: the split (D4), then app and render, chrome and input, navigation and session, agent pane and drawer, preview and control | 20-48 | 400-960 | |
| 8 | Sidecars (D3) | 4-8 | 80-160 | |
| 9 | Pages and files | 3-5 | 60-100 | |
| 10 | Agents: the launch recipe, launchers and skills | 2-4 | 40-80 | |
| 11 | Release gate: the full hosted matrix once, install on every box, the maintainer's use, the tag (0.7.0) | 3-6 | 60-120 | |

Total: about 70 to 145 session hours, 1,400 to 2,900 dollars, most of the range in the frontend and the capsule.
Wall time is longer by the hosted runs and the maintainer's iterations; a second pass in parallel on a disjoint subsystem (in a
worktree, merged when its contract settles) shortens the calendar, not the work.

## Integration tests: which pass reduces each

Each pass keeps one fast unit test per guarantee in its contract and its part of the one smoke per OS; an
integration scenario that sleeps on the clock goes with the fix it pinned. `rust/backend/tests` (26 thousand lines)
and `rust/log/tests` (27 thousand lines) are assigned by file:
- Distribution 1b: `shell_dial`, `unit_pin`, `same_version`, `window_start`.
- Rows: `capsule_workspaces` (5.7k lines, the row half; the capsule half goes with pass 6).
- Messaging: `comm_wake`, `comm_file`, `comm_send`, `hub_link`.
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
- Step 0 landed 2026-10-20. Pass 1a landed 2026-10-20: the release path's contract is the first settled. Pass 1b, the
  installers, is next. Nothing is half-done on main.
- Findings go to the owning subsystem's queue and are handled at that subsystem's pass.
