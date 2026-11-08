# Known defects and open items

The one list of known defects and open items, grouped by the subsystems `package.md` names. A pass reads its own
subsystem's part; no session loads this file at start, and no `@` include may name it. A session files an item
here, and a landing that fixes one changes it in the same commit: the item moves to Closed as one line.

## Form of an item

`- **ID** [mark] text`. The id is UPPER-KEBAB and never reused. The text says what was seen, where (file and function;
line numbers go stale), and the fix when one was decided; it holds no investigation log. The mark says how the item is
known, as the root `CLAUDE.md` defines a known defect:

- `[person]`: seen in use by a person.
- `[session]`: seen by a session operating the product as a user.
- `[note]`: not seen in use; a note from reading code, or a red in a gate or a hosted run with no use behind it.
- `[feature]`: a wish, not a defect; it needs an explicit go-ahead before anyone builds it.

## The check

The local gate runs `scripts/tests/test-defects-check.sh` in every run, scoped or not. It fails with the id when a
landing record names an item that is not in this file. Landing records are the section "Landing records since
2026-11-06" of `docs/history/passes.md` to the end of that file. An id is a token of capitals and digits joined by
hyphens, with at least two parts that each hold two or more capitals, bounded by anything but a letter, digit,
underscore or a hyphen that joins more: `ROW-ID-MINT` and `WIN-IDLE-STATUS-50MS` are ids; `P1`, `P1-P4`, `B1-B5`,
`ALLDONE`, `RCG_SCOPE`, `SHA-256` and `UTF-8` are not. An id is known when an item line starts with it, in an open
section or in Closed. The check also fails on an id that starts two items, on an open item with no mark, and on a
tracked `CLAUDE.md` that cites this file in the form `docs/defects.md: ID` or `docs/defects.md item ID` after an opening parenthesis with no
known id, or with none. It does not prove that a fix moved its item to Closed; that is the landing's duty. The rule is
owned by `docs/CLAUDE.md`.

## platform

- **ENSURE-CONTAINER-TEST-ONLY** [note] `host::durable::ensure_container` (`rust/platform/src/host/durable.rs`) has only
  test callers now that the voyage folder is gone; delete it with its tests.
- **TEST-ISOLATED-TESTS-PORT** [note] The socket suite deleted with the lane servers held tests of
  `sot_platform::test_isolated` itself that have no counterpart in `rust/platform/src/test_isolated_tests.rs`:
  `privileged::{native_role, startup_failure_reports_status_and_capture, entry_failure_never_counts_as_native_proof,
  native_failure_retains_status_and_output}`, `read::{setup_and_read_failures_have_distinct_captured_phases,
  real_reads_preserve_eof_data_and_original_deadline}` and the diagnostics harness cases. Port them from
  `5e5197bcf:rust/log/tests/socket_unix/`.
- **TOML-READERS-FOUR** [note] Four hand-written TOML readers disagree (hosts, rows, settings, resume).
  `docs/ownership.md` already makes platform the owner and the `toml` crate the one parser; each owner converts its
  reader in its own pass.
- **PLATFORM-WINDOWS-GAPS** [note] The Windows `preflight_volume` has no test; `resource_dir` and the native launcher are
  owned APIs with no guarantee test; Windows durable removal has no parent sync.
- **DURABLE-WRITERS-LEFT** [note] Writers that still do not use platform's durable publish, left to their owners' passes:
  files `io.rs` and `concept.rs`, topology `store.rs` and `relay_units.rs`, agents `folder_trust.rs` and
  `account_links.rs` (which also needs a Windows no-clobber move), messaging `registry.rs` and `identity/self_file.rs`.
- **HOST-NAME-SECOND-READERS** [note] Second readers of the raw host name remain for their formats:
  `comm/identity/context.rs` (handles), `updater/src/lock.rs` (lock owner) and the window's resume file name. The
  window's two config-folder rules need a migration (fe-ui persistence).
- **PER-POINT-ARM-SERIALIZES** [note] The hold tool's in-process arms are exclusive per point even when their keys differ
  (`hold.rs`), so a test that fails while armed makes the next test arming that point fail with "another arm of it did not
  end within 30s", naming the wrong cause. Exclusive per point and key would fix the class; only in-process users meet it.
- **CHILD-RENAMES-NO-RETRY** [note] Three test children rename a file into place with `std::fs::rename`, which does not
  retry on Windows while a scanner holds the new file, as platform's renames do: `test_barrier.rs`, the capsule's
  `tests/supervisor/main.rs` and `contained_job.rs`. Route each through `publish_noreplace`.

## wire

- **WIRE-PREVIOUS-SHAPE-TESTS** [note] Ten op families have no previous-shape test (agent, browse, lane, monitor, pages,
  pty, repl, update, workspace, the preview payloads); the defaulted-field rule is a maintenance obligation serde does
  not enforce.
- **WIRE-BLOB-BLOCKING-TWIN** [note] `write_frame_blocking` has no blob path and so no twin of the async writer's blob
  check.
- **LANE-BUILD-DELETE** [note] Delete `DaemonVersion.lane_build` and `sot-fe version`'s build words in the release after
  0.7.0; no protocol-3 reader passes hello any more.

## topology

- **RELAY-SOCKET-HANG** [person] On one user's hub the relay to a daemon-less host showed eof,
  `systemctl --user is-active sot-host-relay-<host>.socket` hung twice for over two minutes, and the user's session
  restarted its `sotd.service` 36 times and killed a "duplicate" daemon while debugging. Hypothesis, unproven:
  `systemctl --user` over a non-interactive ssh without the user bus environment waits out D-Bus timeouts, and the 36
  restarts fit `Restart=on-failure` with a duplicate daemon holding the lock, 150 s each. Main bounds each systemctl call
  but the original cause is not established; needs a real ssh login and user manager.
- **HOSTS-SYNC-TIMEOUT** [session] An ssh install on a Windows box ended with `topology sync --hub <hub>
  failed (exit 2): ssh <hub> did not exit within 30s`, while the box's window link to the hub was up. Likely the install
  login has no key or agent for the hub. Read at topology's pass; the fix is a clear reason in the failure, not a longer
  bound.
- **ONE-LOGIN-CONSTANT** [note] Two constants hold one fact, "the window waits 30 s for one ssh login": the lane
  dial's bound and the control connection's `SSH_LOGIN_BOUND`. Make it one constant.
- **UNITS-WITHOUT-LINKS** [note] Units `sotd` writes should carry no `[Install]` section: boot start comes from a
  `default.target.d` drop-in plus the host condition, so `systemctl --user enable` and `disable` change no file and
  `hosts.toml` alone decides where each unit runs. This closes a hand-typed `disable` on one host of a shared home
  disabling the unit for every host. `sotd.service` is done; the relay socket units
  (`rust/protocol/src/topology/relay_units.rs`, `rust/backend/src/topology/relay_units.rs`) still carry `[Install]`, and
  the generated tunnel template and boot drop-ins are not written. A partial rule leaves the hazard on the tunnel links.
- **PARKED-SPARE-CLOSE** [note] Closing a gate does not end a parked spare login until it is used or its endpoint drops
  (`rust/topology/src/lane_client.rs`, `start_spare`, `take_spare`). Test: keep the endpoint, never take the spare, see
  the child end after close.
- **HANDSHAKE-PIPE-HOLDER** [note] The lane handshake reads stdout blocking and cancels the leader alone (`lane_child.rs`;
  `platform/src/identity/deadline.rs` waits for the body): the class containment's `Signal::output` bounded.
- **REFRESH-DEADLINE** [note] `SYSTEMCTL_BOUND` is per call and a refresh makes `2 + 3N` calls, so a stuck user manager
  can cost minutes.
- **RELAY-OPTION-SETS-TWICE** [note] `RELAY_COMMAND_LINE` and `SSH_OPTS` are two spellings of one option set, the relay
  unit has no login bound, and the shell bridge turns connection sharing on (`comm/lib/comm-lib-client.sh`).
- **STORE-NO-FSYNC** [note] Topology `store.rs` `write_atomic` does not fsync; platform's durable publish would.
- **SYNC-HOSTS-PATH-LITERAL** [note] Sync fetches the literal `~/.config/sot/hosts.toml`; the no-argument stdio bridge
  ignores the endpoint overrides, and its failure report can print twice.
- **TOPOLOGY-SOURCE-TEXT-TESTS** [note] `endpoint_tests.rs::the_windows_launcher_uses_the_label_this_module_derives` and
  the launcher test's "a call in Update-SotRemoteDial" case read source text; replace them with behavior.
- **TOPOLOGY-EDIT-RACE-RENDEZVOUS** [note] The concurrent-edit test lacks a rendezvous; the two-process sync race rests on
  platform's lock.
- **TOPOLOGY-DAEMON-HALF-REACH** [note] The daemon half of topology still reaches server, rows and the lease record; it
  moves behind handlers with the op table.
- **CLIENT-SELF-ENROLL** [feature] A laptop adding itself to the hub's `hosts.toml` during install has a trust
  question (a client writing the hub's host list). Today the docs say the hub-side command and the install fails loudly
  with it.

## containment

- **B15-REVIEW-NOTES** [note] `end_group` with a leader-exited group (`jobs -p` would miss it): the case has a test
  row (`leaderexit`), and the Git Bash stale-group hazard goes with the shell client's removal. `in-job.ps1`'s
  handle-based wait (C#) needs a Windows test computer. The gate's tool probes sit above its container check (add a
  probe-refusal test), and seven background steps started outside `set -m` should use the stub rule.
- **CONTAINMENT-UNTESTED** [note] No test covers: a failed request in `Held`'s Drop; the async `wait` keeping only the
  probe error; the fallback descriptor sweep above a lowered limit (`rust/platform/src/host/fd.rs`); the guard's
  deadline and an undeliverable kill (`rust/containment/src/guard.rs`); Windows console-group isolation of `interrupt`;
  macOS signal delivery to a real daemon; real folder removal off Linux. The Windows `Watched` is duplicated
  (`contain_tests.rs`).

## lifecycle

- **SHUTDOWN-BOUND-TAIL** [note] The backstop starts the terminal at `SHUTDOWN_BOUND`, the terminal's fire and folder
  waits add up to 4 s, and the backstop is armed after the listener's teardown (`lifecycle/shutdown.rs`,
  `server/listen.rs`). Charge the tail inside the bound, with a test of the process's exit
  (`rust/backend/tests/daemon_lifetime/outcomes.rs`).
- **RETIRE-TESTS-ASSUME-THREAD-RUNS** [note] `lifecycle/retire_tests.rs`'s 400 ms bound and its `RETIRE_WAIT` bound (through
  `terminal_retiring`) assume the retirement's step thread is scheduled within them; `retire` returns at its deadline whether or not that thread has run, so a loaded
  host can break them (the same assumption the chat stop test made, CHAT-STOP-STATUS-UNDER-LOAD). Not seen red.
- **CLAIM-EXIT-OWNER** [note] `Signal::claim_exit` belongs to lifecycle's terminal, not containment.
- **LEASE-REJECT-NO-DEADLINE** [note] The lease's malformed-line refusals use the undeadlined `reject` in a loop that
  continues (`lifecycle/lease.rs`), so a lease peer that never reads can fill its buffer; wire's `reject`
  (`sot_protocol::reply::reject`) is unbounded, as are `hand_off`'s `bad_request` and every other `reject` (`conn.rs`),
  though one small frame fits the buffer and none was seen to park.
- **CONVERGE-ENDS-ROWS-WINDOWS** [session] Under a 0.6.6 supervisor, a converge's stop-before-apply on a Windows box ended
  the box's sessions (the daemon logged `ended=2`), twice. A later update by the 0.7 launcher kept a row, so the case
  read as only the 0.6.6 launcher's; not proven. Needs a real Windows 0.7-to-0.7 converge that records job membership,
  since supervisor spawning keeps a no-breakaway fallback (`rust/backend/src/rows/spawn/detach.rs`).

## server

- **ADMISSION-OP-SCAN** [note] `rust/backend/tests/admission.rs` `op_names` lists the ops by scanning `ops/mod.rs`'s
  source, against the rule that no test proves code by scanning its text; read the op table through code (an exported
  list of op names).
- **FINISH-DISPATCH-BYPASS** [note] `file.download`'s pre-stream failure (`files/transfer.rs`) and reauth's errors
  (`rows/reauth/mod.rs`) bypass `finish_dispatch`, so an oversize error closes the connection instead of answering
  `envelope_too_large`.
- **RUN-ERROR-DROPS-STATE-LOCK** [note] An error return from `run` drops the state lock before the terminal runs
  (`server/mod.rs`, `main.rs`).
- **TWO-STARTERS-ONE-SOCKET** [note] Two starters on different state roots can race on one socket path
  (`server/listen.rs`).
- **ACCEPT-SELECT-DEPARTURE** [note] The accept select can take a ready connection while the departure is also ready
  (`server/listen.rs`).
- **PROBE-TRICKLE-UNTESTED** [note] No test trickles a probe reply one byte at a time.
- **WIN-PIPE-OPEN-NOT-REPLY** [note] On Windows a pipe open proves no reply (`server/listen.rs`), and the inbound buffer
  size is only a hint.
- **SERVER-ROOT-REACHABLE** [note] The G14 rule beyond the private submodules is a convention: root-level server items stay
  reachable inside the one crate until the services are crates.
- **SUCCESSOR-START-PROBE** [note] Code comments cite this as "queue item 21". A successor `sotd` started while a child of its killed predecessor is
  mid-exec (still holding a copy of the old listener) could read the old socket as live and refuse to start. Unobserved;
  the window ends at the child's exec and a supervised start retries. Candidate fix: the start probe counts a listener
  live only when it replies, not on a bare connect.

- **REPL-AWAY-NOT-TOLD** [note] While the window used last is away, a `wglshow(fig)` browser frame opens on no window
  (`repl_frame_for` takes `in_use().ok()`), and the REPL run that served it is not told: the collected output carries no
  browser frames, so `sot-fe repl run` returns ok and the session cannot say why no figure showed. Rule to build if seen:
  the run's collected output names the away computer. Sidecars takes part.
- **ROSTER-NO-REAP** [note] A lease or lane connection that has left the loop is not in the roster, so nothing reaps one that
  never pings; the lease has its own ticker and the lane its pipe's end (`conn.rs`).
- **ACCEPT-STOP-UNTESTED** [note] The accept loop's stop before any row is touched (lifecycle's guarantee 5) has no test that
  connects after the departure; only the order of `run_local`'s tail holds it (`listen.rs`, the break, then
  `drop(listener)`).
- **INLINE-DISK-OPS** [note] `INLINE` still holds ops that wait on a local disk (`file.write`, `file.delete`, `dir.create`,
  `concept.write`): on a network filesystem they hold the loop, and with it the ping (guarantee 10's limit).

## rows

- **WORK-STATE-HOOKS-READ-FILES** [note] The work-state shell side reads and writes daemon-owned files by hand:
  `comm/work_state/hooks/comm-status-heartbeat.sh` gates on the registry file existing, `comm-status-idle.sh` gates its
  mail check on the inbox being readable, `comm-status.sh` writes turn, question and dialog markers, and
  `comm-turn-auditor.sh` reads and writes its signature under `comm/state/`, which messaging's contract claims. With those
  paths unreadable the hooks skip work their subcommands could answer. Route them through `sotd work-state` and
  `sotd comm`, and split the Stop script's mail check from its work-state half.
- **WIN-FENCE-PRECLAIM** [note] On Windows the daemon spawns the supervisor directly (`rows/spawn/detach.rs`) and the
  supervisor takes its fence after it starts (`supervisor/mod.rs`), unlike Unix, which claims it before the fork. A
  supervisor that exits in that window leaves no fence file, so the watchdog's first-start recheck and boot resume read
  the row `stopped`. Windows retirement also releases the proof fence before its rename (`rows/state_folder.rs`). Claim
  the fence before the spawn and hand it over, as Unix does.
- **DIALOG-ROW-STAYS-GREEN** [session] A row sat on a Claude Code permission dialog for about 31 minutes in its first turn
  and stayed green; the contract says it reads red within one dialog tick (5 s). The reader (`screen::dialog_of`)
  recognizes a capture of that dialog, and a scratch row with a real permission dialog read red within 6 s, so the fault
  is in that row's reads (phase not Ready, an attach refused, a screen past its 3 s wait) or an unreadable registry, all
  of which skipped silently. A diagnostic now logs each dialog write at info and the first unread tick of a run with its
  reason. Next: when a row stays green on a dialog again, read the `comm dialog:` lines in `sotd.log`. A feature was
  asked beside it: after a dialog stands about a minute, the daemon files one line to a configured handle, once per
  dialog.
- **REAUTH-TRUST-LOST** [session] After 23 rows were moved to another account with `sot-fe reauth` within 20 s,
  7 of the 23 roots had no accepted entry in the new account's `.claude.json` and 3 sat at Claude Code's folder-trust
  dialog. Seen again on 0.7.0 (2026-11-06 and 11-07, three roots, two accounts), with every trust outcome logged. Cause,
  measured there: a lost update. The daemon logged `Recorded` for each root before its leg started; Claude Code's backups
  minutes later held no entry; Claude Code 2.1.319 saves the file under its own lock by reading it again and renaming its
  copy over it, and the daemon's write (`agents/folder_trust.rs`) is outside that lock, so a neighbor leg's start-up save
  replaced it. All three losses came 0.58 to 0.81 s after another leg on the same account started (3 of 6 records written
  within 1 s of a start were lost, none of 15 written 1.44 s or more after one). Not built: when the dialog reader sees the
  folder-trust dialog on a leg's first screen and the root lies under the declared prefix, record trust again and
  restart that leg once with the same resume id (no turn has run); a second sight leaves the row red. The restart waits
  until 2 s have passed since the last Claude leg start on the same trust file, since its own record is the same
  unlocked write; the dialog's variant for a folder already trusted (option `No, continue without these permissions`),
  which a restart cannot clear, is not restarted. Never answer the dialog by keys. Red tests first: a trust entry removed
  after the spawn's record leaves the row restarted once and trusted; a root outside the prefix is never restarted.
- **WRAPPED-DRAFT-READS-DIALOG** [note] A person's draft that wraps (or holds a newline) puts the cursor's row out of the
  box's one line, so `input_refused` returns "not in an input box" and `page_reply::refusal_name` names it `dialog`; the
  person is told a dialog is open when it is their unsent text. Rule if seen: a box identified by its two rules with text
  rows between reads as a draft, the quote reads the cursor's row, and a multi-row box is named "input not empty".
- **WAKE-TESTS-FAIL-UNDER-THE-PARALLEL-GATE** [note] In one local gate run at load 21 to 25, four `comm_wake` tests failed
  (one because `workspace.create` answered "capsule supervisor spawn failed: the durable parent is gone; restart the
  daemon", three past their 30 s bound) and passed one at a time minutes later. The durable parent is marked gone only
  when its channel ends (`rows/spawn/durable/proxy.rs`, `route`), and the test keeps no daemon log, so why it ended is
  unread. In use a daemon whose durable parent dies would start no capsule row until restarted. Log the channel's end
  (the error, the parent's exit status) and keep a failing wake test's daemon log in the gate's folder.
- **ROW-RESTART** [feature] One restart action for a row in phase `terminal` or `ended_no_respawn`: daemon op
  `workspace.restart` owned by rows, `sot-fe restart <ws> [--resume <id>]`, Shift+r in Sessions; same workspace id and
  handle, a fresh leg, resume by id on request. Recorded as unbuilt in the rows contract.
- **PTY-INPUT-MARK** [note] Distinguish `pty.input` from a non-window connection with a visible mark. Until then, approval
  "typed in that session's pane" without `[sot-page]` is a convention, not proof of physical keyboard input.

## capsule

- **CAPSULE-ATTACH-DURING-SPAWN** [note] A driver's attach resizes the terminal through the run's loop
  (`RowRun::resize_terminal`, bounded at `INPUT_BOUND`, 2 s), and the loop answers only once it runs, after
  `spawn_producer`; the supervisor sets the run on the row before the spawn. A window that attaches as driver while the
  agent's terminal is still spawning waits up to 2 s and is refused ("the terminal did not take the resize in time"),
  then redials. Seen once on a hosted Windows runner, where a ConPTY spawn took over 2 s. Cost in use: a refused attach
  and a redial during a slow start, never a lost row. Options: let the attach's resize wait for the spawn under its own
  bound, or record the size for the spawn to apply.
- **WIN-IDLE-STATUS-50MS** [note] A hosted Windows run failed
  `supervisor::lifecycle::a_status_probe_against_an_idle_supervisor_is_not_poll_bound` with one probe of 20 at 50.08 ms
  against its 50 ms bound (the others about 1.2 ms); not seen in the next run on the same code. The shape to look for is
  a wait of one 50 ms tick: a probe that finds no free pipe instance waits in `WaitNamedPipeW` until the server makes
  the next one, or a server path tied to `LOOP_TICK`. Read the row server's Windows accept (is the next instance created
  before the accepted one is served?) and measure 100 probes on a Windows box.
- **CAPSULE-SWEEP-TEST-UNDER-LOAD** [note] `headless::a_sweep_of_watcher_reads_writes_nothing_and_stays_bounded`
  (`rust/capsule/tests/fe_client/main.rs`) read outcome "ending" where "ended" was expected, after 75 s, at a local gate
  with peak load 51.72; it passed at peak loads 20.55 and 11.64. Nothing in those commits touches the capsule. Likely a
  wall-clock bound under another user's load, unproven. A hosted runner at a quarter speed could meet the same bound.
- **CAPSULE-RESUME-TEST-UNDER-LOAD** [note] `resume::capsule_headless_input_resumes_a_row_whose_supervisor_died`
  (`rust/backend/tests/capsule_workspaces/resume.rs`) failed at a local gate at load 73, peak 78: `pty.input` still answered
  `capsule_not_ready`, phase `unreachable`, at its 60 s bound. The same commit passed it in 16 s at peak load 15.7. Nothing in
  that commit touches the capsule. Likely the same kind of wall-clock bound as CAPSULE-SWEEP-TEST-UNDER-LOAD, unproven: a green
  rerun does not exclude a recovery that can stick, and a gate on a shared host hits this when another user's load peaks.
- **MACOS-AGENT-WATCH** [note] macOS has no parent-death signal, so an agent that ignores SIGHUP can outlive a SIGKILLed
  supervisor. Fix: a kqueue watcher. The ordinary case is proved on macOS; the SIGHUP-ignoring case on Linux and Windows
  only.
- **SCREEN-WRITER-SPAN** [note] The screen file's writer is waited for at most 2 s at a run's end, and a writer that timed
  out can still publish a stale snapshot over a later run's file.

## messaging

- **WAKE-EATS-UNSENT-PROMPT** [person] A mail wake typed into a row whose prompt held a person's unsent line
  replaced that line, so the instruction never reached the session; seen again on the 0.6.6 daemon, where a typed answer
  to a permission question was gone. The wake promises a free prompt only. Main has a test that a row holding unsent
  text is never typed into, and the wake logs every typing at info, but the cause is not known: one loss was Claude
  Code's own input redraw with no mail in flight. Needs a real Claude Code row, its version, a screen capture and the
  wake log together.
- **WAKE-WINDOWS-READ** [note] On a Windows box a typed wake line failed to read alone within the op budget
  (later reads matched it), and a tick refused "cursor not at the prompt" with the line in the box. Needs a captured
  Windows screen of a Claude box holding typed text; the assumption that typed-text and NBSP attributes survive ConPTY
  is in `rust/backend/src/comm/wake/screen.rs`. A named limit in `comm/wake/mod.rs`.
- **REPLY-WRAP-WINDOWS-UNPROVEN** [note] The wrapped-line gate rests on Claude Code's layout captured on Linux (2.1.312). On
  Windows a different layout may type the line and then withhold Enter (`typed_not_sent`, the line left in the box).
  Capture a wrapped box on a Windows box and add it as a fixture.
- **HUB-OFFER-SILENT-LINK** [session] A send through the hub reads `FAILED -> @h: the daemon on <box> did not answer within
  3s; @h may still be filed` whenever a linked box leaves the offer unanswered, though no daemon that could hold the
  handle was silent (the hub offers a handle its folder does not list to every linked daemon at once,
  `rust/backend/src/mail/links.rs`). Fixed for the case seen: one link per computer, and a silent verdict lists the
  unlinked boxes. Open: the hub cannot say which box holds a handle. A rule that judged by the handle's suffix was
  withdrawn, since a row can be created with any `agent_name` and a link's declared host can differ from the raw host.
  An honest fix needs authoritative ownership (the linked box tells the hub which handles it holds) or a silent link
  found dead and dropped before the offer; a design exists for a read-only query before one filing, a new wire
  operation for after 0.7.1. The cause of the silent link (a stale link from a box that slept is likeliest) is unread.
- **M3B-SENDER-NOTICE** [note] A row that keeps refusing the wake should tell each sender of its unread mail, once
  per streak. The review's blockers are the design input: it fires for every busy row, and there is no route to a sender
  the folder does not list.
- **SUBAGENT-NOMAIL** [note] Decision 0031's "a subagent uses no mail" cannot be a gate today: on Claude Code
  2.1.303 an in-process subagent's Bash shell and its session's own shell carry the same `CLAUDE_*` names and values and
  the same process ancestry. The rule is an instruction; a gate needs a harness signal (an agent id in the subagent's
  environment) to exist first.
- **COMM-HOME-TWICE** [note] The comm folder is computed twice: Rust `comm/home.rs` and the Julia installer's
  `src/homes.jl`. One owner; the installer asks `sotd comm`. The other pair named by the messaging pass is `fmt_age`: its shell copy in
  `comm/lib/comm-lib-base.sh` is still called by `agents/sot-fe/sot-fe-version.sh`.
- **COMM-SHELL-LEFTOVERS** [note] Left from the shell client's removal: `registry_replace --text` has no caller; the shell
  `with_lock` trap test needs a holder or goes with the shell client; `comm/inbox.rs` calls rows' macOS mount check
  (platform's fact); `comm-status.sh`'s identity, agent gate and markers and the hooks move to `sotd comm`.
- **GUARD-PROFILE-LOUD** [note] `comm/tests/lib-home-guard.sh` reads the account's profile silently on Windows: when
  `whoami.exe` fails, the registry is unreadable or `ProfileImagePath` is an unexpanded `REG_EXPAND_SZ`, a suite runs under
  a swapped HOME with neither the profile nor the account recorded. Where `cygpath` marks Windows, make the guard fatal on
  an empty or non-absolute profile, naming the source. Written against the shell client; not re-read against main.
- **WIN-SID-ONE-OWNER** [note] The Windows SID is read twice, by `_sot_windows_sid` (`comm/lib/comm-lib-client.sh`) and
  the home guard, spelling `whoami`'s path and parsing it two ways; one owner.
- **NATIVE-PATH-SAYS-WHY** [note] `sot_native_path` (`comm/lib/comm-lib-client.sh`) fails silently, so a `cygpath`
  failure reaches `sot-fe`, `comm-context` and the routable check as the wrong reason; one stderr line there names it for
  every caller.
- **FE-ENDPOINT-ONCE** [note] `sot-fe` resolves the endpoint twice for `docs` and `repl run` (in the dispatcher and
  again in `sot-fe-request.sh` and `sot-fe-repl.sh`); resolve once in the dispatcher.
- **SHELL-HIDDEN-OUTPUT** [note] `agents/tests/test-sot-fe-paths.sh`'s `lib()`, `sent()` and `probe_root_sent()` discard
  the child's stdout and stderr, so a FAIL reads `got ''`.
- **SEND-BODY-ON-STDIN** [note] The send command should take its body on stdin (`@h -`) so a message never passes
  through a shell's quoting; today single quotes are only documented.
- **COMPACTION-SUPPORT** [feature] A handoff command and template, a `/handoff` skill, read-back in the reset
  hooks, a usage tap in the statuslines and an installer default for `autoCompactWindow`; a port of the unmerged
  handoff branch, not a merge.
- **BASH32-RANGE-LOCALE** [note] Under bash 3.2.57 with a glibc collation locale (`en_US.UTF-8`), a `[a-z]` range in a
  pattern also matches `A` to `Y`, so the endpoint check in `comm/lib/comm-lib-client.sh` (the `ssh:` value's host)
  accepts `ssh:Hub`. Seen 2026-11-07 when the endpoint gate suite ran with that bash first on PATH and that locale; the
  gates run with no locale and pass. Whether macOS's `/bin/bash` behaves so is not known. The same range appears in
  `agents/sot-fe/sot-fe-row.sh` and `agents/worktree/comm-worktree-new.sh`. Fix: name the characters or use a class, in
  each owner's pass.
- **COMM-SUITES-BASH-CLAIM** [note] `comm/CLAUDE.md` says `under-bash.sh` runs every hermetic suite under bash 3.2.57;
  `under-bash.sh` runs only `scripts/tests/` suites with that bash and the others with the bash that started it (the
  gate's and CI's are newer), the scripts under test reaching 3.2 only through PATH. The page should say which.
- **WAKE-REATTACH-BOUND** [note] The wake's run-fence test (`an_attempt_never_types_into_a_run_it_did_not_check`) waits
  the attach client's redial cap plus 1 s after the new run shows, so that the attempt's own client is on the new run
  before the test releases its hold: one order kept by a bound from a product constant, not a hold. A hold point where the
  capsule's attach worker completes an attach would force it.

## agents

- **SPAWN-REMOTE-FOLDER** [session] No verb starts a row on another computer from the hub for a folder only the far
  computer has: `comm-spawn.sh` checks the folder locally (`[ -d ]`). Running the spawn on the far box over ssh works around it.
- **PLACED-COPY-SKILL-RERUN** [note] Every edit of a placed-copy skill (sitrep, show-result) makes the gate's
  `julia-root` fail until `scripts/legacy-skill-hashes.sh` is rerun, because removal needs the shipped copy's hash in
  `comm/legacy-skills.txt`. A test that names the rule would say so at the edit.
- **SOT-FE-WINDOWS-UNBOUNDED-READS** [note] By reading, on the no-endpoint path of `sot-fe` in Git Bash on Windows:
  `sotd.exe session-socket-path local`, the pipe probe's `sot_dial` (in `_sot_windows_local_pipe`) and
  `_sot_windows_sid`'s `whoami.exe` and `powershell.exe` in the hello run without a bound of their own. The seen hang
  was a different cause (fixed); bound these with generous bounds, each failure naming its step.
- **WIN-LAUNCHER-HELD-JSON** [note] The Windows launcher's freshness step (`scripts/sot-freshness.ps1`) calls the stop
  helper, which derives and reads the daemon's `held.json` itself (`scripts/sot-local-daemon.ps1`) instead of asking the
  daemon; agents' guarantee 9 names it as outside its scope.
- **REMOTE-TOOL** [feature] Run a registered tool (first: a CAD package's STEP export over COM) on a named Windows
  window box from the hub, in the logged-in desktop session, with no Claude session there; return exit code and log.
  Candidates: a daemon-brokered allowlisted window verb, or the window's own `sotd` running it as a child. The sizing
  covers the allowlist, decision 0031's isolation guarantee and the tool's limits (desktop session only, one at a
  time, COM start hangs). Interim: send mail to a Claude row on that box.
- **CLAUDE-MD-STUB** [note] On a Windows box without symlink privilege a named account's `CLAUDE.md` is a seed copy,
  because the import stub (`@<default CLAUDE.md>`, so the global instructions stay one file across accounts) could not
  be proven without a logged-in account. Once an account is logged in there, a one-minute check (a marker line in the
  default `CLAUDE.md` that `claude -p` in a stubbed account echoes) decides whether the stub replaces the copy.
- **WIN-SKILL-HASH-CRLF** [note] The Windows skill-folder hash: a CRLF checkout's copies differ from the list and are kept.
- **MEMORY-FLAG-UNGUARANTEED** [note] The auto-memory flag (`memory.rs`, `--settings` key `autoMemoryDirectory`) has no
  guarantee of its own in the agents page; it is owned policy of the recipe, proved by `memory::tests::*`.
- **CODEX-GLOBAL-SKILLS** [note] Codex's skills and plugin still install into the global `$CODEX_HOME` and `~/.agents`,
  outside any row; the same question as guarantee 7, for Codex. A pass decides it with the Codex plugin mechanism.
- **TRUST-WRITE-DROPS-SAVE** [note] The reverse of REAUTH-TRUST-LOST, by reading: `ensure_folder_trusted`
  (`agents/folder_trust.rs`) reads the account's `.claude.json`, inserts the entry and renames its copy over the file
  outside Claude Code's lock, so a Claude Code save that lands between that read and that rename is dropped (a counter, a
  project field, or an account field). Not seen; the window is a few milliseconds.

## files

- **WIN-SLOW-RUNNER-TESTS** [note] On a hosted Windows run the shard's unit run took 201 s against 35 s on the last green
  run, and four tests failed: three `io_trash_tests` waiting 30 s for a PowerShell child to write its ready file, and
  `no_argument_or_variable_chooses_a_handle` with a blank stdout; the rerun passed all 1,000. A slow runner, with 90 s of
  the 166 explained. Unread: why the join test came back blank when slow. A test that needs a bound should fail naming
  it, so print what `join` said on stderr in the assertion.

## sidecars

- **JULIA-RELATIVE-PATH** [note] `julia.rs`: an empty or relative PATH entry can give a relative answer, and a broken
  juliaup default falls through to PATH. Fix: refuse a non-absolute candidate; decide the default's fallthrough.
- **KERNEL-QUEUED-DEAD** [note] `kernel.rs`: submissions still queued when a kernel generation dies get the generic
  channel error (`kernel_request_failed`, not `kernel_unavailable`). Fix: drain the queue with `Dead` as `pending` is.
- **KERNEL-IDLE-EXIT** [note] `kernel.rs`: the service loop has no `wait_until_exited`, so a descendant that holds the
  stdout of a dead idle kernel keeps it from being replaced. Fix: a select arm on the leader's exit.
- **PLUTO-START-EXIT** [note] `pluto.rs`: nothing observes the leader's exit before `READY`, so a dead leader whose
  descendant holds stdout reads as starting until the readiness bound; the start's construction is outside the
  supervisor's deadline; the cleanup's reap is unbounded. Fix: observe exit in `await_ready` as the serving loop does;
  bound the reap.
- **DEADLINES-IN-CALLER** [note] `pluto.rs`, `mathjax.rs`: the deadlines live in the caller's future, so a caller
  canceled after its request was accepted leaves none on the supervisor; MathJax's blocked write and its death detection
  (stdout EOF) ignore the `Signal` and the leader's exit. Fix: supervisor-owned deadlines; select the `Signal` and
  `wait_until_exited` around writes.
- **MONITOR-SCRIPT-DELIVERY** [note] `monitor.rs`: the sampler script's delivery to the child has no deadline, is not
  polled for the `Signal`, and precedes the silence clock, so a stalled ssh start that fills the stdin pipe is never
  replaced. Fix: race it against the `Signal` and a bound, keeping the child for checked cleanup.
- **MATHJAX-READY-LOSSY** [note] `mathjax.rs`: a lossily decoded first line fails MathJax's ready check. Fix: compare
  the parsed ready object.
- **STARTUP-TAIL-IN-ANSWER** [note] `kernel.rs`, `mathjax.rs`: startup errors omit the captured stderr tail from the
  answer.
- **PLUTO-OPEN-VALIDATION** [note] `ops.rs`: no regular-file check or deadline before the header read, so a FIFO inside
  a row's root parks `pluto.open`; the header check is a substring. Fix: `metadata().is_file()` first and an exact-line
  compare; pin the file by descriptor.
- **CLEANUP-PROOF** [note] `update.rs`, `child/tmp.rs`, containment's `signal.rs`: canceled updater calls drop the
  folder without a reap, removal's result is not awaited, and `kill` has no wall-clock ceiling. Fix: containment's.
- **NO-HEREDOC-PROOF** [note] `monitor.rs`: no test proves the sampler script avoids here-documents; a behavioral proof
  needs a leak check under bash 5.2.
- **TIMEOUT-DOUBLE-DONE** [note] `repl/execute.rs` closes a timed-out run's drawer entry with a synthetic `done`, and
  the shim's own `done` follows (`ShipToolsRepl.jl`, forwarded by `supervisor.rs` `route_line`), so such a run can show
  two. Fix: one completion owner per run, the supervisor, with the caller's timeout only ending its wait.
- **ROW-END-REPL** [note] `rows/ops/destroy.rs` removes the registry entry and leaves the children alive while a handler
  holds the workspace (an `execute` waiting at `repl/execute.rs`), so destroying that row neither answers the run nor
  ends its child, display or grants. Fix: row end retires the REPL explicitly through rows' end API.
- **RETIREMENT-ERROR-DROPPED** [note] `repl/mod.rs` `Slot::settle` drops a finished retirement's error, after which a
  restart starts a child although `child/tmp.rs` kept the folder because termination failed. Fix: keep the failed tree
  owned until it is confirmed gone (containment's), or refuse the replacement.
- **COLLECTOR-LIFETIME** [note] `repl/supervisor.rs` keeps a `repl.execute` collector until `done` or the response, so
  it goes on collecting after its caller timed out. Fix: drop the collector with its caller.
- **COLLECTION-BOUND** [note] `ExecAccum::push` bounds only text frames. Fix: bound the frame count and bytes of every
  kind.
- **READY-ANNOUNCEMENT** [note] `supervisor.rs` flips `starting` to `ready` on the child's first stdout line, parsed or
  not. Fix: flip on the parsed `repl.ready` (or the first parsed envelope).
- **END-NOT-RETRIED** [note] `ports.rs` `PortSink::end`: a failed `End` is logged and not retried, so a receiver that
  refuses it leaves the grant. Fix: the receiver's failure contract, when a receiver can fail.
- **CI-PRE-REPL-INTERRUPT** [note] The testset "signals after a forced stop meet a child held still" in
  `julia/repl/test/interrupt_child.jl` (`run_shim`) sends exactly five signals 50 ms apart and expects Julia to force the
  stop, with its held-still window placed from the fifth. On Linux Julia counts a signal only when the eval's thread runs
  between two; on Julia 1.12.7 with two cores shared with six busy loops 20 of the testset's 32 checks failed and the
  child stayed silent until the test's 240 s timer (it passes on an idle computer on 1.12.7 and 1.13.0), which fits
  signals that merge and a loop never forced; merging is likely, not proven. The daemon's `Escalation` is a different
  sequence (it signals every 50 ms for `FORCE_SPAN`, then answers the failure). The pre-release Julia's first-testset
  failure was not reproduced. The three `run_shim` signal testsets run only with `SOT_REPL_SIGNAL_TESTS=1` (the local
  gate and `release-gate.yml`'s `repl-signals-linux`), not in `CI.yml`. Fix: drive the test's signals as `Escalation`
  does, place the held-still window from the child's first line, then return the testsets to `CI.yml`.
- **DISPLAY-INTO-CHILD** [note] `sidecars/display/` (one 235-line file and a page) moves to
  `sidecars/child/display.rs`, the concept "what each child the daemon starts carries"; it deletes a folder, a page and
  the "display/ sibling" cross-reference.
- **WIN-CIM-LISTING-30S** [note] `sidecars::repl::project_tests::the_command_line_observer_rejects_a_deliberate_leak` failed
  once on a hosted Windows run: the whole-table `Get-CimInstance Win32_Process` listing in `repl/tree_argv_tests.rs`
  `read_tree` ran past its 30 s bound; the rerun passed. The state the test does not own is the runner's WMI service.
  Query only the probe's tree (`-Filter` by ParentProcessId, walked), or name a bound for a cold provider; measure a cold
  first query on a Windows box.
- **PKG-DOWNLOAD-TARBALL** [note] A Pkg download tarball was left in `TMPDIR`; the root cause was not proven, so
  newer child-folder cleanup cannot be said to fix it. Needs a Linux Julia/Pkg interrupted-download setup.
- **REPL-SURVIVES-DAEMON** [feature] A row's REPL (and kernel) outlive a daemon restart, so a row keeps its loaded
  packages and hardware state across an update; today a daemon stop ends its Julia children (three restarts each ended
  the REPL on one instrument box).
- **SIDECAR-TEST-APPARATUS** [note] `spawn_sotd_as_user_service`
  (`rust/backend/tests/support/env.rs`, systemd-run) does not go through `sotd_command`, so a daemon that path starts gets
  no `SOT_JULIA_BIN` from `julia_binary()` and its kernel can run a juliaup launcher under the test's `HOME`. Reds
  nothing today.
- **HOLDER-CONFIRMATION** [note] `child/display/holders.rs`: signals are not confirmed deaths, and unreadable environments
  and failed pidfd opens are only logged. Fix: report the count that failed and wait for the pidfds with a bound.
- **DISPLAY-HOLDERS-W2-W3** [note] W2: end every process that holds a user-code child's display at that child's every
  end (restart, death, row end), not only at a display's end; W3: a cgroup per child (ADR 0050's containment, which is
  containment's and unbuilt). W2 changes accepted behavior (a detached process the REPL started would no longer outlive
  it; ADR 0050 residual 7 records orphaned Pluto workers), so both go together.
- **MATHJAX-KEEP** [note] D3's MathJax question: keep the node child only if markdown with math is previewed in use. The
  MathJax code is unchanged until the question is answered.

## pages

- **QUARTO-RENDER-FOLDER** [note] Quarto's per-render folder outlives a killed render; its detached engine is
  reused for 300 s, so a sweep must hold the engine's life, not the render's. `run_quarto` (`pages/ops.rs`) owns
  process termination but no engine-lifetime scratch cleanup. Needs real Quarto and an interrupted render.
- **REDIAL-POOL-STEAL-RACE** [note] A redial racing a pool steal can lose its port (harmless; the page reopens).
- **LINK-WATCH-SHAPE** [note] `admit` should return the child together with its watch, both taken under the
  admission lock; that deletes the microsecond gap before admission and two of the three `gate.watch()` calls, and
  `pages.rs`'s `pipe_child`/`pipe_one_over` lose their `Option<LinkWatch>` (a default gate's watch, which never closes,
  serves the three tests that pass `None`).
- **QUARTO-START-BOUND-STARVED** [note] `quarto_never_holds_a_freed_group_number` (`pages/ops_quarto_shutdown_tests.rs`)
  failed 4 of 30 runs at its five-second start bound ("the stub render never started") under extreme CPU starvation in a
  review's 50-run loops, 2026-11-07; the bound was unchanged by the fix to the test's sleeper.

## fe-net

- **REDIAL-LATE-ACCEPT** [note] A run accepted after a redial's hello, during a REPL start (up to about 15 s), is not
  named by that hello; the limit is recorded.
- **SSH-MUX** [feature] One ssh connection per computer carrying every pane: a window on another box opens two
  fresh ssh logins per pane attach, and Win32-OpenSSH offers no multiplexing, so the per-host connection would be our
  own stream multiplexer between the window and `sotd stdio-bridge` (frames with stream ids and per-stream credit,
  version skew detection, a new ADR; 8 to 10 files, about 1,500 to 2,500 lines with tests). The login cap per host
  waits on a count of sshd's dropped logins.
- **BUS-LOST-NO-RECOVERY** [note] A connection that falls behind a bus gets one typed `bus.lost` frame naming the bus and the
  count; an older window or linked daemon ignores it, and no pane recovers from it yet. The project chat is its first
  consumer.
- **HUB-TELLS-OLDER-WINDOW** [feature] At the 0.6.6-to-0.7 crossing a 0.7 hub cannot make an older window install: an
  older window answers a refused hello only with its overlay, and its bits come only from its own updater or a manual
  stage. Unfixable for 0.6.6 boxes. Build it so the next crossing works: the hub's hello refusal names its exact release
  (it already carries `backend_version`), and a window or its launcher, on a refusal from a newer hub it is declared to
  follow, stages that exact tag through the installer's verified path and converges with no paste. Open: the source of the bits when the release is a draft or the box cannot reach GitHub (the hub serving its own
  verified pack over the window's link, admitted to the same OS account only), a pinned install, and an instrument
  computer (never mid-acquisition: the converge waits for the person or asks). Server and distribution take part.

## fe-ui

- **KEYS-DROPPED-WHILE-A-ROW-ATTACHES** [person] Keys typed while a row's attach is pending were discarded ("intermittent
  freezes, keys discarded") during a load average of 110 on 32 cores. The key loss is fixed: keys typed while a row
  connects are held in a visible line on the pane, held per row with an 8 KiB bound, and sent only by a fresh Enter one
  second after the row's screen was presented, into the run it showed. Open: why a first attach closed about 100 ms
  after it opened (bursts of "capsule attach episode failure ... status=redialing" while rows were switched every
  200 ms, and single ones). The attach end records now log stage, cause and full error; read them at the next
  occurrence. Beside it: a client installed but not yet attached forwards input with no run identity, so it can land in
  a run the person never saw, against the pane's own rule; the discard notice clears on attach and can go unseen; a
  client dropped with unsent input has no accounting.
- **REPLY-BAR-DELETE** [note] The window's reply bar and `page.reply` over the window's connection serve only a daemon from
  before v0.7.0-rc.2: the page server refuses an unscoped reply (403 `no_reply_scope`) and `fe.command.send` refuses
  the reserved `page_reply`, so nothing a current daemon sends reaches the bar. While it stands, a reply confirmed just
  as the link drops can wait in the window's outgoing queue and be written on the next connection. Delete the bar,
  `OutgoingReq::PageReply`, `send_page_reply`, `IncomingEvt::PageReplied` and their tests, with a red test against the
  real transport.
- **EXIT-PROCESS-WINDOWS** [note] `ui::app::exit_process_tests::unarmed_return_arms_the_fallback` failed once on a hosted
  Windows run ("the owned process survived past its 4.5s bound in scenario fallback-err"), green on the rerun, with no
  frontend change. Cause not found; the three-second exit backstop is in `rust/frontend/src/ui/app/exit.rs`. Needs
  Windows under comparable load.
- **CONCEPT-BODY-UNDRAWN** [note] The concept slot's shaped body is built and resized but never drawn; it looks deletable,
  with its build and resize calls. Not re-read against main.
- **RESUME-FILE-BACKSLASH** [note] The resume file's writer escapes backslashes and its reader does not unescape, so a
  backslash doubles at every save. Found by reading at an earlier head; not re-read against main.
- **FOCUS-SETTER-TEXT-SCAN** [note] A text scan guards the focus setter; the window keeps six maps keyed by row. `State`'s
  fields are D4's revision on the package page.
- **LAYOUT-ONE-POSITION** [note] The width pre-pass and the layout loop in `rust/frontend/src/ui/chrome/layout.rs`
  each keep their own running position with the same skip rule; storing each column's x position in the list would
  remove the loop copy.
- **P7-RESULT-LEDGER-HELPER** [note] Retiring many rows clones every pending result id through `session/badge.rs`'s helper,
  whose only caller is `workspace_list.rs`. Move the reconciliation beside the pending-result owner, test identity
  membership, delete the helper, and keep the pending-only guard.
- **TERMINAL-DIM-BOX-ARMS** [note] DIM never dims a box-drawing cell's arms: border quads take the glyph color undimmed.
- **QUAD-SRGB-DARKER** [note] Every solid quad (borders, selection, code panels) samples an sRGB texture and shows darker on
  a surface that does not encode sRGB on write.
- **PANE-CONNECT** [feature] Warm the pane path at window start: a new window's first pane spends 0.5 to 0.65 s in
  text preparation (the first glyph-atlas fill) and its first dial has no parked spare; hosted macOS first panes were
  1.26 to 1.45 s against 0.13 to 0.93 s later. An improvement, not a defect.
- **SCRATCH-PAD** [feature] A drawable preview pane: the requirement "The agent sees what the user draws" in
  `requirements.md`, approved by the maintainer 2026-11-07. Decided then: a drawing travels the selection's road (the row
  has one slot, text or a drawing); the window draws the ink and the daemon composites the picture the agent reads with
  one small rasterizer both use, in wire; drawings are not kept as files (the window keeps each row's drawing; the picture
  is written inside the project for the agent); the left button draws and the right erases whole strokes. Owners: wire
  (the shape and the raster), files (the picture), agents (the slot), fe-ui and fe-net (input, ink, sharing). Not built;
  the window's pieces follow the selection's window side.

## distribution

- **INSTALL-OBSERVE-071** [note] For 0.7.1. The branch `pass/install-observe` holds the observation rebuild (`Fact<T>`, the
  Machine seam and fake-machine phase tables, the decision table, a verdict that drops no reason, every question bounded)
  and our four subcommands answering the installer as one JSON value of a shared serde type. Three blockers remain: the
  bash unit form's flattened `argv[]` cannot fix Bash's argument boundaries (`role.rs`; read the unit some way that keeps
  argv boundaries, or refuse the bash form's ownership as unknown); `--json` argument errors print no JSON
  (`same_version.rs`, `update/cli.rs`); a multiline reason reaches the verdict unchanged (`verdict.rs`; render one
  line). Also: tests that drive `decide`, the update and the final checks with a cut listing, and the remaining text
  readers of our own output (session-socket-path, update direction, `--version`). Rebase onto main first; the 0.7.0
  minimum changed `procs.rs`.
- **INSTALL-STATE-EMPTY-EXPORT** [note] With `~/.bashrc` exporting an empty `XDG_STATE_HOME`, a home on NFS and the
  installer's own `state.conf` in place, `state::setup` (`get_nonempty`) treats the empty export as no choice and approves
  the scratch folder, while the daemon, which sources `~/.bashrc`, resolves the empty export to the NFS default. Treat a
  present-but-empty export as an explicit choice of the default in `setup`, or name it in the verdict.
- **OLD-SUPERVISOR-DESK-STEP** [session] A window whose launcher predates the code id gets the update armed and a desk step
  (quit with Keep, start from the shortcut), so a box whose account is signed out cannot be updated remotely
  (`rust/backend/src/install/supervisor.rs`, `windows.rs`). Applies to 0.6.6 and earlier launchers; the 0.7 installer
  asks the daemon for its children, which answered correctly on a Windows box. Open only for those older windows.
- **INSTALL-1B-LEFT** [note] Left by the installer pass: the Unix end check's callers have no test (`unix::daemon_after`
  takes the whole `Plan`, which has no test constructor, and runs systemctl from PATH); several guarantees still carry
  first-round tests (the record, the Julia floor, the option names, folder trust with no guarantee); the Windows
  launcher's log words that `sotd install` reads are a seam no test holds (`docs/integration.md`, the
  supervisor-record row); the launcher code id's file list is tested only through the id's behavior
  (`test-install-layout.ps1`).
- **RETIRE-06-DELETE** [note] In the release after 0.7.0, delete `rows/spawn/retire_06.rs` with the 0.6 journal reader, its
  tests, its fixtures and the `.retired-voyages` sweep step; distribution then refuses an install that jumps a 0.6 box
  straight past 0.7.0, or keeps the retire. Rows takes part.
- **OPS-DELETE-GUARD-POWERSHELL** [note] The PowerShell half of the delete guard is unbuilt (`scripts/sot-apply.ps1`, near
  its removal steps). Found by reading an earlier head; not re-read against main.
- **LOCK-PID-PROBE** [note] The updater stage lock (`rust/updater/src/lock.rs`) and `sot_log_protected`
  (`scripts/lib/sot-daemon.sh`) trust a pid probe, so a reused pid reads as a live holder. The stage lock is bounded (a
  12 h takeover by age; a refused acquire names the lock and its owner); a reused pid keeps one old log from pruning. A
  held-lock form needs a design for macOS (no `flock` command) and PowerShell (.NET `FileStream.Lock`).
- **JQ-PROVIDE** [feature] Bundle or download `jq` with the install, so a box without it needs no manual step. Every
  installer already checks for jq and names the install command, and a send or poll without jq prints FAILED naming jq.
- **CONVERGE-HOLDS-LEASE** [feature] A converge started over ssh holds its own window lease across the window's
  exit and sends Keep once the window is gone, so a box whose supervisor predates the lease can be updated remotely with
  its sessions kept. Premise to test first: an rc9.12 daemon grants a lease to a same-user ssh process.
- **WIN-SHELL-TEST-HARNESS** [note] One Windows shell-test harness (a scratch user, the machine variables, native and Git
  Bash paths, permissions) for every Windows shell and installer witness, in place of per-test fixture fixes: the
  publish-guard passes each lost one to two hours to per-test Windows fixture rounds.
- **SETUP-GUARD-WINDOWS-SPEED** [note] On hosted Windows each real Git Bash enrollment by `scripts/setup-publish-guard.sh`
  takes tens of seconds to minutes (four in parallel: each guard unit test 9 to 11 minutes), part of it the script's
  PowerShell calls. An install enrolls two clones. Measure one enrollment on a real Windows desk, count the PowerShell
  starts per run, and fold them into one or into the installer's Rust.
- **GUARD-ADVERTISEMENT-NOTES** [note] All minor. (A) `scripts/hooks/pre-push`'s comment says the walk refuses a first
  field that is not an object id, but it tests the excluded-id file before `oid_ok`, so an empty, `.` or `..` field is
  skipped, not refused (harmless); swap the two checks so the comment is exact. (B) On Windows a POSIX-form destination
  path handed to the hook is converted on its way to git.exe, so the `--get-url` equality can refuse falsely (fails
  closed). (C) Optional deletions: `tracking-unrelated` overlaps `tracking-overlap` and `tracking-pushurl`;
  `ls-remote --refs` would drop HEAD and peeled lines; the old-id queue is covered by the advertisement.
- **SHARDS-REST-EXCLUDE** [note] `shards.py`'s rest-0 builds `--lib` with an `--exclude` for every package whose library
  the table names (`selection_of`), and in the same cargo call selects the unnamed test targets by name; a test target
  of an excluded package then fails as "no test target named ... in default-run packages" on every hosted system, while
  the local gate and `shards.py check` stay green. The three moved platform tests were named in the `plat` shard
  (9453f035b). Open: make `check` refuse an unnamed test target whose package rest-0 excludes, or split rest-0's library
  selection into its own cargo call.
- **WIN-JOB-RECORD-HANG** [note] Twice a hosted Windows job's log ended with its tests ok and "Cleaning up orphan processes"
  yet its record stayed `in_progress` for tens of minutes while the run read completed success. A watcher that waits on
  every job's record hangs. Find whether the runner's orphan step waits on a process our tests leave; until then a run
  whose every job log ended green is read as green. Needs a hosted Windows runner.
- **MAC-RUNNER-SPEED** [note] Hosted macOS runners ran at a half to a quarter of their usual speed on 2026-11-03 (build
  4m12 against 1m05 with a full cache hit), and three jobs failed on time with no test failed; the same was seen on
  hosted Windows (`window_startup_hardens_handles_before_starting_children` past its child's absolute deadline). Every
  step of the build+test job now has its own bound and the job ceiling is their sum. Open: headroom that names the
  runner (a ceiling that excludes the build, or a slow-runner note in the verdict), not a raised bound; the release
  gate's native pane timing bound is the same class.
- **CI-UBUNTU-2604** [note] GitHub moved `ubuntu-latest` to 26.04. There the publish guard's bash 3.2.57 build fails
  (`shell_version_string` implicitly declared is an error under the newer compiler) and the sotd shard fails host-dependent
  tests (rows create rollback, the registry lock, sidecar folder removal, trust-config restore, site verbs). Linux jobs
  are pinned to `ubuntu-24.04`. Make each pass on 26.04 (one diagnosis per failing test: the state it does not own),
  then move the pin on purpose.
- **RUST-YML-PUSH-FILTER** [note] `rust.yml`'s push filter omits `core/**` and `julia/**`, which its Rust tests exercise; a
  271-line catalog proves a 24-line runner (a source-spelling test).
- **SECRET-SCAN-AFTER-PUSH** [note] The secret scan (`.github/workflows/gitleaks.yml`) runs only after a push and reads every
  pushed branch, and the pre-push publish guard does not scan for secrets, so a real token committed on a branch is public
  before anyone sees it. Seen when a made-up Slack app token in a test reached four pushed branches and turned main's scan red.
- **GATE-OWN-SUITES-HOSTED-ONLY** [note] The local gate (`scripts/tests/rc-gate.sh`) never runs the gate's own suites
  (`scripts/tests/test-rc-gate-*.sh`); they run only in the hosted `rust.yml` and `release-gate.yml` after a push, so a change
  to the gate lands on whole runs that cannot see its own cases fail. Seen 2026-11-07: a commit whose scope case failed
  passed two whole runs. Fix: run them in any run whose scope holds a gate script.
- **GATE-NO-RELEASE-PROFILE** [note] No local gate job and no hosted run before a tag compiles the release profile, so a
  release-only compile error is first seen at the tag build (`release.yml`). Seen 2026-11-07 in review: a hold point's
  release expansion did not compile; the hold tool now expands one form in both profiles.
- **GATE-KEEPS-NO-FAILED-LOGS** [note] The local gate deletes its log folder at each run's start, so a failed job's log is
  gone once the next run starts. Seen 2026-11-07: a run failed the sotd unit tests' first part (`sotd__sotd-c-1`, exit
  101) once, the runs before and after it were clean, and the next run had deleted its log, so the failing test is
  unknown. Fix: keep each run's summary and every failed job's log under a folder named by the commit.

## chat

- **CHAT-FLOOD-CAP-UNDER-LOAD** [note] `socket_tests.rs`'s
  `a_write_that_cannot_finish_retires_the_socket_within_the_write_bound` fails on a Linux host under CPU load, in two ways.
  Under 64 busy loops its flood reached its frame and byte caps without one write stalling ("the flood never stalled a write
  within its caps"), and later runs passed. Pinned to one core beside eight busy loops it fails every run, at the base and the
  head, with "a stalled write held the socket ... never dropped". Both are plain panics, so the pause check does not cover
  them; whether the second is the fixture's or the product's write bound is unknown.
- **CHAT-SECOND-ASK-UNPINNED** [note] `chat_tests.rs`'s `a_stop_that_does_not_finish_is_reported_incomplete_and_final` claims
  "a second ask neither stops again nor reads as retired", but it reads the listener's stop count before a stop spawned by
  the second ask would be polled, so a product that stops again passes. Pin it by an order (the hold tool), not a sleep.
- **CHAT-TRANSIENT-WAITS** [note] Five chat tests wait with `eventually` for a state another task holds only for a bound
  and then leaves, so a test not polled for that long misses it and fails: `service_tests.rs`'
  `stop_is_bounded_final_and_repeatable_and_the_lock_waits_for_the_listener` and
  `a_second_concurrent_stop_returns_at_once_and_the_status_reaches_stopped` (`Stopping`, about 2 s and 1 s), and
  `socket_tests.rs`' `a_gap_opened_by_an_outage_survives_a_reconciliation_made_during_it`,
  `a_reconciliation_begun_before_a_hello_is_refused_by_it` (the refused reopen, until the connect after a 400 ms pause) and
  `stop_during_the_reconnect_pause_does_not_wait_for_it` (no socket open, for a 5 s pause). Not seen failing. Pin each by
  an order (the hold tool), not a window.

## records

- **FOUR-NUMBERS-TWICE-NAMES** [note] `scripts/four-numbers.jl` should list the twice pairs a pass adds, by name, at each
  landing, as it lists removed tests (23 of one pass's 25 added twice names were door wrappers and generic names).
- **FOUR-NUMBERS-REMOVED-TESTS** [note] `scripts/four-numbers.jl` lists the tests a pass removed, by name, and the report
  pairs each with the guarantee test covering it or the code deleted with it (about an hour; a short agent at a boundary,
  never beside another agent).
- **DOCS-VERSION-PICKER** [note] The `gh-pages` branch's `versions.js` still lists every old version folder (v0.3.0 to
  v0.6.2, the v0.6.0 rc folders included) and sets `DOCUMENTER_NEWEST` to 0.6.2, so the picker offers old manuals beside
  stable and dev. Main now delegates version generation to DocumenterVitepress; the deployed site must be inspected.
- **DOCS-ENV-REFERENCE** [feature] An environment-variable and command reference page.
- **DOCS-ROADMAP** [feature] A real roadmap page (the old one is retitled as the historical Phase 1 plan with a
  status table).
- **DOCS-GENERATED-TABLES** [feature] Generated reference tables (keybindings, config, ops) so they cannot drift.
- **DOCS-STALE-CHECK** [feature] A lane check that flags manual pages a merge makes stale.
- **PLATFORM-FACTS** [note] Four facts are in no folder page (an MSYS exec's dead Windows parent, the prompt glyph on
  Windows, antivirus holding a fresh file, the background service); each goes into its owner folder's `CLAUDE.md`.
- **PROCESS-NAMES-IN-REPO** [note] About 160 lines of the tree's comments and docs name the lab's working roles and
  sessions; the public-repo rule wants no process or people in repo text. A records-only sweep stating each rule plainly.
- **DANGLING-POINTERS** [note] Nineteen code comments cite an `isolation-plan.md` and one a "supervisor-epoch design
  section 7", neither in the repo; also bare dated citations of a decision and "this sprint" with no referent, "round-2 item N"
  markers in `topology/status.rs` and `dial.rs`, `output_end.rs` citations of a decision, ADR history naming a source by
  a session name, and a tool name in `.gitignore`. Also code comments that cite "queue item 21" (`rust/backend/src/server/listen.rs`, `listen_tests.rs` twice,
  `rust/backend/tests/live_socket.rs`) and "queue item 58" (`rust/backend/tests/capsule_workspaces/work_state.rs`), a
  list that is not in the tree; cite `docs/defects.md` ids or state the rule. Each cites a document in the repo or states its rule plainly.
- **CORE-UNWIRED-TYPES** [note] `core/` has five unwired types and three unused imports.

- **UNMERGED-BRANCH-IDEAS** [note] Work on branches that never merged, kept as ideas by owning subsystem; none is a
  lane. Messaging: an unreadable registry reads nothing; a retired process named by pid and start; every shell client
  names a refused hello; protocol wording for the single writer; a Rust `comm_send` test from an old patch. Server and
  fe-net: a second account refused without the kick; a Windows owner lookup and page-port scope text; downloads written
  through `write_frame_to`; one error frame per failed `repl.eval`; a lane handshake seam in test builds only.
  Lifecycle: children ended at shutdown with a suite that was red at the stop; a quit bounded on the OS clock (check
  against the window's three-second teardown). Rows and agents: a row restarting in place on its own account;
  `SOTD_BIN` trial edits. Sidecars: signal a kernel's group once and close queued submissions. Distribution: a done-test
  probe; installer hooks (superseded, check); stop commands that match only what they stop.

## Closed

An item closes by moving here as one line: id, subsystem, the fixing commit or landing.

- **WIN-SAVE-HELD-OPEN** (platform) closed by 2658a328d and 4ec7bced6, 2026-11-08: on Windows the write beneath a root renamed with the classic replace alone, so a save of a file another program held open with delete sharing failed where `std::fs::rename` saved it; the rename now falls back to a POSIX-semantics replace by the same handle when the classic one answers access denied, and a link at the final name, to a file or a folder, is replaced on every system.
- **CHAT-WATCH-STALE-SLOT** (chat) closed by 585800d2f, 2026-11-07: a chat watch dispatch test failed on macOS in 2 of 6 hosted runs ("timed out waiting for the stale slot released by the owner") because the owner's snapshot could check the row before the test removed it; the snapshot marks `chat.watch.snapshot` and the test removes the row while it is held.
- **BENEATH-SUPERSEDED-WRITER-ERRS** (platform) closed by d79396550, 2026-11-07: the write beneath a root no longer checks the final name after its rename, so two writers of one name each answer Ok; Ok means the entry at the temporary name was renamed into place.
- **GATE-LEFTOVER-ENDPOINT-STUBS** (messaging) closed by 4ed49a71d and 61874b765, 2026-11-07: the endpoint gate suite's stubs left their `sleep 30` children running, and a gate whose last jobs included that suite failed its census; each stub now starts in its own group and is ended with it.
- **HOLD-CHILD-PASS-TORN** (platform) closed by bf04197f6 and fe6291036, 2026-11-07: the hold tool's child and the exit-process fixture's child wrote a file a test polls for by create and then write, so a test could read it empty (seen on macOS in a hosted run); both now rename a whole file into place.

- **CHAT-STOP-STATUS-UNDER-LOAD** (chat) closed by the landing that fixes it, 2026-11-07: a test race; `retire` reports at its bound before its thread reaches a late `request_stop`, and the test now reads what the stop sets only after the stop began, forcing that order once per run.
- **ROW-CLEAR-NO-SESSION-START** (agents) closed by the session-start hook landing: a Claude row leg's `--settings` (for legs started after the update) carries SessionStart on `clear` and `compact`, running the read-only comm session start, whose output names the handle and the handoff. Not covered: `/resume` typed in a live row, Codex rows.
- **DESTROY-SESSION-LIMBO** (fe-ui) closed by the landing that fixes it, 2026-11-07: destroying the session in view left the window on the default row, or, when another window or the command line destroyed it, on a session that no longer exists, and a session destroyed elsewhere kept its parked view, REPL state and tree slots; the view now moves to the next session in the Sessions list (below, else above, across computers), the pane is empty when none remains, destroying any other session changes nothing, and a row a listing no longer has loses its parked view, REPL state and tree slots.
- **ROW-ID-MINT** (rows) closed by f7905b1a5: a row id is unique within a process by count, not by the second.
- **GATE-TARGET-SHARED** (distribution) closed by d74ddbe77: the gate holds a lock on its target folder, and a second gate on the same target fails naming it.
- **LANE-BRIDGE-RELAY-HALF-CLOSE** (capsule) closed by 0f7cdf875: the throttled relay half-closes each destination as its copy returns.
- **INSTALL-WIN-UNBOUNDED-STEPS** (distribution) closed by 7cd579500, merged at 3678eaf20: the Windows process listing and end-state version asks are bounded at 60 s, and a listing with no answer is never "none runs".
