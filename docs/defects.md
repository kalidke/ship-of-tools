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
- **NOREPLACE-NFS-EINVAL** [note] `publish_noreplace_refuses_an_existing_target_and_leaves_both_files` fails with `EINVAL`
  when `TMPDIR` is on an NFS (nfs4) home, seen 2026-11-08: `renameat2` with `RENAME_NOREPLACE` is refused there, with no
  existing target. The callers of `publish_noreplace` are agents `trust_declaration.rs` (`settings.toml` in the config
  folder) and `install/state.rs` (the `sotd.service` drop-in `state.conf`, published only when the default state root
  is refused); both write under the user's home and neither falls back on `EINVAL`; neither has been run on an NFS
  home.
- **DURABLE-WRITERS-LEFT** [note] Writers that still do not use platform's durable publish, left to their owners' passes:
  topology `store.rs` and `relay_units.rs`, agents `folder_trust.rs` and
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
- **BENEATH-TWO-WRITERS-WINDOWS-RECURS** [session] `two_writers_of_one_name_each_publish_a_whole_file` failed again on a
  hosted Windows run after BENEATH-TWO-WRITERS-WINDOWS was closed (run 43306919232, 2026-11-12, at main 1a9f96c17, whose
  change touched no platform code): a writer's `write_beneath` answered access denied (`beneath_tests.rs:524`). The
  earlier fix closed the renamed file's handle before the folder's sync; another holder of the name is still possible
  (a scanner, or the other writer's rename), so the retry window or the share mode does not cover every case. The
  passing runs before it say it is intermittent; its cause is unknown.

## wire

- **WIRE-PREVIOUS-SHAPE-TESTS** [note] Ten op families have no previous-shape test (agent, browse, lane, monitor, pages,
  pty, repl, update, workspace, the preview payloads); the defaulted-field rule is a maintenance obligation serde does
  not enforce.
- **WIRE-BLOB-BLOCKING-TWIN** [note] `write_frame_blocking` has no blob path and so no twin of the async writer's blob
  check.
- **LANE-BUILD-DELETE** [note] Delete `DaemonVersion.lane_build` and `sot-fe version`'s build words in the release after
  0.7.0; no protocol-3 reader passes hello any more.
- **PAGE-REPLY-UNSAFE-CHAR-PARTIAL** [note] `page_reply::unsafe_char` (`rust/protocol/src/page_reply.rs`), which the
  chat drawer also uses to clean names, does not list the deprecated format characters U+206A to U+206F, the soft
  hyphen, the tag characters (U+E0001, U+E0020 to U+E007F, which can carry hidden text) or U+180E, while
  `docs/integration.md`'s page reply row says it refuses "invisible formatting characters". None breaks a line. Found by
  B5f's reviews, reading code; never seen. Chat's mail body (`sot-chat` `mail::body`, B3b) passes them on to the agent
  that reads it, so mail can carry text a person reading it does not see; B3b's review's probe built such a body (tag
  characters, U+00AD and U+180E all present). The fix is wire's: one rule for every reader of the text grammar, its own
  landing.

## topology

- **RELAY-SOCKET-HANG** [person] On one user's hub the relay to a daemon-less host showed eof,
  `systemctl --user is-active sot-host-relay-<host>.socket` hung twice for over two minutes, and the user's session
  restarted its `sotd.service` 36 times and killed a "duplicate" daemon while debugging. Open: the hang is not
  reproduced and its cause is not established. The hypothesis that a non-interactive ssh without the user bus
  environment waits out D-Bus timeouts is refuted (systemd 255 and 259): with the environment, without it, or with no
  manager a call returns in 0.01 s, and a manager that accepts and never answers fails it at 90 s. Two minutes equals
  the session tool's default timeout, which fits a command cut by that tool but is a guess. The 36 restarts are
  explained (SOTD-UNIT-RESTART-LOOP), and their daemon-reloads could have overlapped the hang, but nothing shows that. The
  eof for a computer with no daemon is by design (the bridge's reason line goes to the hub's journal). What would
  settle it, from the hub for that hour: `journalctl --user -u sotd.service -o short-precise` (restarts 2 s or 152 s
  apart), the user manager's journal around the hang, `systemctl --user show sotd.service -p NRestarts`, the
  `daemon.lock` holder's command line and socket, and the exact command the session ran with its own timing.
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

- **ESCAPED-HOLDER-MACOS** [note] A hosted run on main (2026-11-14, Rust run 43633631706, macOS, shard
  sotd-c+plat+inst) failed `signal::output_bound_tests::output_does_not_wait_for_an_escaped_pipe_holder`
  (`rust/containment/src/signal_output_bound_tests.rs`): `Signal::output` with a 2 s bound returned the command's
  status 0 instead of `TimedOut`, so the escaped holder (a `perl` fork after `setsid`, holding the pipe while `keep`
  exists) did not keep the pipe open past the bound. The test passed on every earlier run that day and nothing in
  containment had changed. Not diagnosed: whether the escapee died early, never inherited the pipe, or the leader's
  wait ran past the escapee's loop.
- **B15-REVIEW-NOTES** [note] `end_group` with a leader-exited group (`jobs -p` would miss it): the case has a test
  row (`leaderexit`), and the Git Bash stale-group hazard goes with the shell client's removal. `in-job.ps1`'s
  handle-based wait (C#) needs a Windows test computer. The gate's tool probes sit above its container check (add a
  probe-refusal test), and seven background steps started outside `set -m` should use the stub rule.
- **CONTAINMENT-UNTESTED** [note] No test covers: a failed request in `Held`'s Drop; the async `wait` keeping only the
  probe error; the fallback descriptor sweep above a lowered limit (`rust/platform/src/host/fd.rs`); the guard's
  deadline and an undeliverable kill (`rust/containment/src/guard.rs`); Windows console-group isolation of `interrupt`;
  macOS signal delivery to a real daemon; real folder removal off Linux. The Windows `Watched` is duplicated
  (`contain_tests.rs`).
- **GATE-LEFTOVER-PER-JOB** [note] The gate's leftover census runs once, after every job, so a test that leaves a
  process fails a whole gate only when its job ends within the process's life of the census: the escaped holder ran
  green for nineteen days (GATE-LEFTOVER-ESCAPED-HOLDER). Running each test binary under a child subreaper that lists
  what it leaves would fail that job every time. Also unchecked: the REPL respawn test's grandchild `sleep 30` stays in
  its launcher's group and is ended with it, but no assertion says it is. Found by that fix's diagnosis and review.

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

- **START-TEST-ENV-RACE** [note] A rows start test reads `XDG_STATE_HOME` without holding the test lock that guards
  the environment, so another test that sets it at the same moment moves the first test's state folder. Seen once in a
  scoped local gate on 2026-11-14 whose change touched only a Windows test helper; the rerun passed. Fix: the test takes
  the lock.
- **ROWS-ROLLBACK-TEST-RACE** [note] `rows::ops::create::tests::rollback_tests::a_destroy_during_a_failing_create_stays_destroyed`
  panicked `no row with task new task` (`create_tests.rs`, its registry lookup) once in a whole local gate run on
  2026-11-10 whose change touched only the window, and passed in the day's other whole runs: a state the test does not
  own, not yet found.
- **STATE-ROOT-CHECK-NEW-FOLDER** [note] `state_root_inside_project` (`rows/spawn/state_root.rs`), the refusal of a row
  whose state folder lies inside its project root, canonicalizes both paths and keeps the raw spelling of one that does
  not exist. A row's state folder does not exist at its first launch, so where the canonical spelling differs from the raw
  one (Windows, whose canonical paths carry `\\?\`; a project root reached through a link) the check can answer "not
  inside" for a folder that is. Read from the code after a hosted Windows run of the trust restart's leg test, whose
  failed spawn on a missing state folder inside the root was not refused there; the test now creates the folder.
  Fix at rows' pass: compare the folder's nearest existing ancestor, canonicalized, with the rest appended.
- **DEGRADE-TEST-ROW-SCOPE** [note] `platform::capsule_launch_degrades_when_no_user_scope_is_available`
  (`rust/backend/tests/capsule_workspaces/`) fails when the test itself runs inside a Ship of Tools row: its witness, "not
  in a `sot-row-*.scope`", reads the row the test runs in as the daemon's. Seen on a build computer, on main and with the
  activation change; green on the hosted runners. Fix: a witness the test's own place cannot fool (not in the scope the
  daemon would make for this row, or in the test process's own cgroup).
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
  within 1 s of a start were lost, none of 15 written 1.44 s or more after one). Built (rows, `run/trust_restart.rs`): the
  dialog tick reads every Ready Claude row whose started leg has not shown its main box, and at the folder-trust dialog with
  the root under the declared prefix stops the leg and spawns it again with the same argv (a reauth's `--resume <id>`
  kept), under the row's guard, once per row until a main box shows; the new spawn records trust again. A second sight, a
  root outside the prefix and the already-trusted variant (option `No, continue without these permissions`) are left to
  their person. The restart waits, tried again at the next tick, while this daemon started a Claude leg on the same trust file
  less than 2 s ago, since its own record is the same unlocked write; the check is made before the stop, and starts by
  another computer's daemon or outside any row are not seen. A stop or spawn that fails leaves the row resting or at its
  dialog. Never keys. Tests: `capsule_workspaces::work_state`
  (a reauthed leg and a watchdog-restarted leg, each with a stub that loses the record) and the unit tests of
  `rows/run/trust_restart_tests.rs`. Open, not closed: those tests use a stub, never Claude Code, so they do not show the
  recovery against the real lost update, the wait against a live neighbor start, or macOS (its dialog is uncaptured; a
  wrong reading fails closed); it closes when a person has seen a lost record recovered in use.
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

- **MACOS-SOCKOPT-RECORDS** [note] Two records of when macOS refuses a socket option disagree. The foreign-peer row test
  (`rust/capsule/tests/row.rs`) rests on xnu's `sosetoptlock`, which refuses an option on a socket shut in both directions;
  `rust/capsule/tests/macos_kernel_facts/peertoken.rs` and `rust/backend/tests/shell_dial.rs` say Darwin refuses
  `SO_RCVTIMEO` on an accepted AF_UNIX socket and honors it on a connected one, while the row server sets it on accepted
  sockets and passes on macOS. Settle the peertoken observation with a macOS run and make the records one rule.
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

- **HUB-LINK-CROSS-LINE-MAIL** [session] A hub and a linked computer on 0.6.6 and on 0.7 carry no mail between them:
  the link's hello passes the one protocol gate (`server/hello.rs` `protocol_gate`), 0.6.6 is 3 and 0.7 is 4, and
  0.6.6's scripts send protocol 3. Seen with the published 0.6.6 and 0.7.0 daemons through `comm/tests/interop-066.sh`
  part 2, in both pairings: the link drops every 30 s and a send answers `FAILED`, never `filed`; part 1 (one shared
  folder) passes. The contract said they carried mail both ways; it now says what holds, and the manual's updating
  page and `docs/UPDATE-AGENT.md` say that linked computers move in one sitting, daemons before windows. Fix still owed: delete
  part 2 of the test (the gate that refuses is right and stays).
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
- **MATRIX-PREFLIGHT-SIGPIPE** [session] The comm matrix (`comm/tests/comm-matrix.sh`, `matrix_preflight`) stopped at
  its preflight with "no frontend attached" for a box whose window line its own output printed: it finds the line with
  `printf | grep -m1` under `set -o pipefail`, and when grep exits at its first match the writer can take SIGPIPE (141),
  so the substitution fails and the line reads empty. Replayed on the same output it failed once in five; a second run
  passed. Fix: a match that reads the whole input (`awk` printing the first match, or no `-m1`).
- **COMM-LIST-WINDOW-NO-SESSIONS** [session] On a Windows window computer, `comm-list.sh`'s last line said the window's
  own handle "declares no sessions" while the same listing showed that computer's rows, so one of the two reads the
  wrong registry. Seen by a session operating the product there under 0.7.1-rc.2.
- **COMM-GUARD-RACES-ENV** [note] The comm-home guard (`rust/backend/src/comm/mod.rs`) catches a test that resolves
  the comm folder from the process environment only when no concurrent guarded test holds `SOT_COMM_HOME` at that
  moment: guarded tests set it under the environment lock and remove it on drop (`agents/support_tests.rs`), so an
  unguarded reader passes while one does and aborts through `HOME` when none does. On 2026-11-10 such a test passed
  the scoped local gate on an idle server and aborted the unscoped gate and the hosted runs under load (the same job,
  filters and home; reproduced: alone it aborts 3 of 3, its shard at 64 threads passes 4 of 4 and at 2 threads aborts
  3 of 3). Safety does not depend on the interleaving: such a test reads another test's temporary folder or aborts
  through `HOME`, and never reaches a folder outside the temporary folder. Only catching it does. Decided 2026-11-10:
  kept as a limit. A deterministic check (abort unless the thread holds the environment lock) was built and not landed,
  since it took about 78 product lines of heuristics (a lock wrapper, an environment judged process-derived by
  comparing values, helper threads exempted by name) and still let a helper thread's read pass; a loaded run, such as
  the hosted ones, catches such a test.

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
- **FILES-CONCEPT-NAME-RULES** [note] The concept store's target rule and platform's name rule differ. A link at the note's
  name that resolves inside the root is replaced by the note, while `file.write` refuses every link at its name; the
  string rule (`target_to_path`) accepts `.` segments and, on Windows, `:`, which the write refuses. Decided: one rule for
  both, at the concept pass.

## sidecars

- **TEST-CHILD-OUTLIVES-TEST** [session] Julia children that `sotd` unit tests start (a kernel's `start.jl` and two REPL
  shims in a project named with spaces) outlived their tests by five hours, two of them at full CPU, reparented and
  still in the row's cgroup, after a run that started each unit test in its own process with a scratch home
  (2026-11-10, ended by hand). Fix: a test that starts a child ends it on every path, including its own process ending.
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
  gate and `release-gate.yml`'s `repl-signals-linux`), not in `CI.yml`. Seen again in the local gate on 2026-11-14 on
  Julia 1.13.0, at a peak load of 13.8 on a lab server: one check failed (the stopped eval's error frames were not
  exactly one "eval interrupted"), the rest of the run green; the same commit's rerun at a peak of 12.0 passed. Fix:
  drive the test's signals as `Escalation` does, place the held-still window from the child's first line, then return
  the testsets to `CI.yml`.
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
- **REPL-FIGURE-SILENT** [note] The reply of `repl.execute` says nothing when a figure is not written; only the daemon log
  does (`rust/backend/src/sidecars/repl/execute.rs` `spill_exec_figures`), and the julia-repl skill tells the agent it gets
  the paths.
- **KERNEL-THIRD-SPAWN-WINDOWS** [session] On a Windows window computer under 0.7.1-rc.1, three `julia.exe` kernels ran
  under `sotd` with no REPL rows, spawned hours apart and then twice within five seconds, the third right after a
  `markdown.tokenize` for the workspace `<default>`: a kernel kept past its row, or a second spawn for one request. Seen by
  a session operating the product there; not yet reproduced; not seen under rc.2's first run.

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

- **MISMATCH-ADVICE-IS-A-DEVELOPERS** [note] The protocol-mismatch overlay ends "dev: git pull + rebuild + relaunch"
  (`net/transport/hello.rs`, `protocol_mismatch_message`). On an installed release the remedy is the installer on the
  computer that is behind, and this is what every 0.6 window shows the day its hub moves to 0.7. Fix: one sentence that
  names the side to update and how.
- **REDIAL-LATE-ACCEPT** [note] A run accepted after a redial's hello, during a REPL start (up to about 15 s), is not
  named by that hello; the limit is recorded.
- **SSH-MUX** [feature] One ssh connection per computer carrying every pane: a window on another box opens two
  fresh ssh logins per pane attach, and Win32-OpenSSH offers no multiplexing, so the per-host connection would be our
  own stream multiplexer between the window and `sotd stdio-bridge` (frames with stream ids and per-stream credit,
  version skew detection, a new ADR; 8 to 10 files, about 1,500 to 2,500 lines with tests). The login cap per host
  waits on a count of sshd's dropped logins.
- **BUS-LOST-NO-RECOVERY** [note] A connection that falls behind a bus gets one typed `bus.lost` frame naming the bus and the
  count; an older window or linked daemon ignores it, and only the project chat recovers from it (its owner watches again after `bus.lost` for `chat.changed`); no other
  pane does.
- **HUB-TELLS-OLDER-WINDOW** [feature] At the 0.6.6-to-0.7 crossing a 0.7 hub cannot make an older window install: an
  older window answers a refused hello only with its overlay, and its bits come only from its own updater or a manual
  stage. Unfixable for 0.6.6 boxes. Build it so the next crossing works: the hub's hello refusal names its exact release
  (it already carries `backend_version`), and a window or its launcher, on a refusal from a newer hub it is declared to
  follow, stages that exact tag through the installer's verified path and converges with no paste. Open: the source of the bits when the release is a draft or the box cannot reach GitHub (the hub serving its own
  verified pack over the window's link, admitted to the same OS account only), a pinned install, and an instrument
  computer (never mid-acquisition: the converge waits for the person or asks). Server and distribution take part.

## fe-ui

- **PDF-TEXT-SELECTION** [feature] Text the person selects in a PDF preview is shared with the row's agent, as a text
  preview's selection is (`requirements.md`, "The agent sees what the user selects"). A PDF page is shown as a picture,
  so it needs the words and their positions per page from the PDF plugin, the window's mapping of a drag to them, and
  the selection's page and text on the existing share. The maintainer asked whether it is listed (2026-11-10).
- **DRAW-ON-PDF-PAGES** [feature] The person's drawing, shared with the agent, on a PDF page as on a picture. The
  maintainer, 2026-11-11, after using the drawing on a picture, a blank canvas and a markdown file: "Would like to have
  on pdfs as well, but not if it delays 71 significantly." A PDF page is not a drawing base today: the window would
  take a shown page as one, keyed by its page, and the daemon would draw the ink over the page it renders.
- **SELECT-MARKDOWN-PINS** [note] Rules of the rendered markdown selection that no test pins, found by its review by
  mutants that pass the whole suite: that the lines between a selection's two ends count (a drag from the blank line
  above a paragraph to the one below shares the paragraph), the preview pane's `maps_source` true in
  `preview/pane.rs` (the one line that turns the map on), the arm in `markdown/buffer.rs` that leaves a map that does
  not line up unshared, that an inline equation's placeholder holds its paragraph's lines (`source_lines.rs`), and that
  `build` skips a mark out of order silently. Deletable: the backend test's copy of the frontend fixture (one
  `include_str!`), the public re-export of `SourceLines`, and a second borrow of the node in `walk.rs`.
- **SELECT-MARKDOWN-FIXTURE-CRLF** [note] The rendered markdown selection's tests read their fixture with
  `include_str!` and depend on `.gitattributes` keeping it LF; a Windows checkout made before that rule keeps a CRLF
  copy (git does not rewrite it on pull) and fails four tests there until
  `rust/frontend/src/ui/preview/markdown/testdata/selection_probe.md` is checked out again. Making the tests strip CR
  where they read the fixture would end the dependence. Found by the line-ending fix's review.
- **INK-REACH-ONE-SIZE** [note] The eraser's reach tests (`rust/frontend/src/ui/preview/image/ink_tests.rs`) run at
  one raster size, so a reach computed from a fixed raster at every size survives them, and
  `the_reach_is_the_ink_and_one_pixel_on_both_sides` repeats the first reach test's positive side. Fix: derive the
  offsets from `ink_radius`, test two raster sizes, and delete the repeat. Found by the thinner pen's review.
- **SELECTION-WITHDRAWN-ON-REFRESH** [note] A change to the shown file, or to the text size, refreshes its preview and
  clears the flowed selection, so the window's share (`rust/frontend/src/ui/preview/share.rs`) withdraws the text
  selection with no status line: an agent that edits the file the person selected loses the selection at its next
  prompt, and the person learns it only by asking. The manual says so. Whether a refresh should keep the selection where
  its lines still hold, or say on the status line that it was withdrawn, is open. Found reading code for the manual;
  never seen. A math, figure or `markdown.tokenize` reply also reflows a markdown preview (`reflow_markdown_preview`,
  `rust/frontend/src/ui/app/frame.rs`) and clears its selection a frame after the share was sent, with no status line,
  which since the markdown selection can withdraw a rendered selection on a document's first view.
- **INK-RASTER-SERVED-SIZE** [note] The window keeps a drawing's raster at the wire's `raster_dims` of the figure's
  served size (`rust/frontend/src/ui/preview/image/ink.rs`); a figure over 20 MB and 6000 pixels is served downsampled,
  and the preview reply carries the factor only into the physical scale (`rust/backend/src/files/preview/mod.rs`), so
  for some sizes the window's raster is a row or a column off the daemon's, which composites at the source's size. The
  ink rides as fractions, so the agent's picture is right; only the window's display differs by a pixel. Fix: the
  preview reply carries the source size and the window takes it. Found reading code; never seen.
- **FE-CONCEPT-SAVE-SILENT** [note] The window shows nothing when `concept.write` is refused: the reply is only logged
  (`rust/frontend/src/ui/preview/replies.rs`) and the editor stays dirty.
- **KEYS-DROPPED-WHILE-A-ROW-ATTACHES** [person] Keys typed while a row's attach is pending were discarded ("intermittent
  freezes, keys discarded") during a load average of 110 on 32 cores. The key loss is fixed: keys typed while a row
  connects are held in a visible line on the pane, held per row with an 8 KiB bound, and sent only by a fresh Enter one
  second after the row's screen was presented, into the run it showed. A warm client the window had parked for a row
  it switched away from stopped reading once 4 MiB of the row's output waited unread, and the row cut it at its 5 s
  write bound; shown again, it replayed the stale output as attached, and a key typed then was lost as delivery
  unknown. Reproduced in a test against a real supervisor and fixed: a client off screen keeps reading and drops the
  output, and asks for the screen when shown. Not reproduced: the pane's "episode failure ... status=redialing"
  warning (a debug build drains the stale output before the warning shows); that this cut is its cause, and the "about
  100 ms after the switch" timing, are inferred from release-build drain rates, and no person's window log was read, so
  the entry stays open for the warning: whether a first attach also closes for another cause is unproven; read the end
  records at the next occurrence.
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
- **CLOSED-DRAWER-KEEPS-FOCUS** [note] After a Terminal spawn failure the drawer is closed but focus stays in the drawer
  slot (`terminal_spawn_failed`, pinned by `terminal_spawn_failure_closes_only_the_drawer`), and `DrawerInput::of(Closed)`
  is the REPL's, so letters go into the REPL's draft and Enter evaluates it. Fix: `terminal_spawn_failed` returns the
  toggle's focus effect (the nav when focus was in the drawer). Found reading code; never seen. The function is in
  `rust/frontend/src/ui/input/layout_help.rs`.
- **B5A-ARMS-UNDRIVEN** [note] The drawer's input decision (`help::DrawerInput::of`) is tested for every tenant, but no
  test drives the handlers that match it: in `rust/frontend/src/ui/drawer/keys.rs` the REPL's handling runs after the
  match, so an `Inert` arm that loses its `return` falls through to the REPL and still compiles, and the wheel's
  `Inert` arm (`input/mouse.rs`) or a call site handed a constant could send a non-REPL tenant's input to the REPL with
  every test passing. Fix: make each arm an expression of type `ControlFlow<()>` (inert, terminal, REPL), so a lost
  `return` is a compile error, and delete the dead tenant check in `terminal_key`; the module doc there still gives the
  old order, and `line_scroll_target`'s doc should say "never another tenant's". Found by B5a's second review.
- **HELP-SEARCH-DROPS-ALTGR** [note] Help's search drops AltGr characters on Windows: its query admits a key only with
  neither Ctrl nor Super (`input/layout_help.rs`, the `Key::Character` arm), and AltGr is Ctrl with Alt there, so the
  window's `chord_types_text` (`input/mod.rs`), which admits it, is not used. Found reading code; never seen.
- **REPL-CTRL-CHORD-TYPES** [note] The REPL's draft types the letter of a Ctrl chord that resolved to no action: its arm
  (`drawer/keys.rs`, `repl_input_key`) blocks only macOS Command chords and does not call the window's
  `chord_types_text` (`input/mod.rs`), which the editor calls. Found reading code; never seen.
- **CHAT-FLIGHT-HOST-UNPINNED** [note] The chat drawer's set of reads in flight matches a reply, and cancels a held
  read, on its own host only (`rust/frontend/src/ui/drawer/chat/drawer.rs`), but no test puts the same read on two hosts,
  so matching on any host passes every test; if it regressed, host b's reply would remove host a's abandoned read and be
  dropped, and b's next equal read would wait until b's link changes. The comment on `Flight::clear` there says "no reply
  will come", which the contract's limit (fe-net's exception for a request sent before a link change is drained)
  contradicts. Found by B5c3's review, reading code; never seen.
- **SETTINGS-PARSERS-DISAGREE** [note] The window's `settings.toml` reader (`rust/frontend/src/ui/persist/settings.rs`)
  accepts what the daemon's TOML reader refuses: `name = Ada` without quotes, and an escaped quote kept with its
  backslash. The daemon reads the same file each time it starts a row, and a file it cannot read leaves folder trust
  unrecorded, so every Claude row on that computer stops at the trust dialog. The chat's `[chat] name` makes a
  hand-edited setting likelier; the manual shows it quoted. One reader for both, or the window refusing what the daemon
  refuses, closes it. Found by B5m's review, probing; never seen.
- **PATH-FOLD-LEXICAL** [note] The window's fold of an agent's path (`nav::files::node_path`) is text only. It does not
  follow a symlink, so `a/link/../x` names `a/x` where the kernel and shell resolve `..` after the link (a result folder
  that is a link into other storage); and it splits on `/` only, so a Windows daemon's `a\b.pdf` is accepted but never
  equals the tree's `a/b.pdf` and never refreshes, and `a\..\b.pdf` is refused and marked. Found by the review of the
  landing that closed PREVIEW-PDF-STALE, probing the first and reading the second; the second never seen.
- **FIGURE-CACHE-KEYED-BY-LINK** [note] The markdown figure cache (`preview/image/figure_store.rs`) is keyed by the
  figure's link text alone, and a figure reply carries no host or row, so two markdown files, in two rows or two
  folders of one row, that name the same relative image share one picture. Known by reading only.
- **PARKED-ROW-PREVIEW-STALE** [note] A file rewritten while its row is not shown keeps its old picture when the
  person comes back, until the cursor moves off it and back: the daemon reports a file's change only for the row on
  screen (`server/events.rs`). Not seen in use; three forms of a fix (refetch on every return, the daemon keeping the
  changed paths of rows it filtered out, or the window sending the shown bytes' version) wait on whether it is seen.
- **SVG-DRAW** [feature] Drawing over an SVG file would need the daemon to render the SVG in a bounded child (as the
  window's drawing child does) before it composes the picture for the agent; not built, so the drawing key over an
  picture the window shows (an SVG, a PDF page, a video poster) says it cannot be drawn on yet (`SVG-DRAW-SILENT`).
  Over an SVG's text (before its picture is drawn, or after a refusal) `d` says nothing, since the `Image` scope needs
  a shown picture; a narrower route of about 10 lines exists (a `Context` flag and a scope for `PreviewDraw` alone),
  left because the state is transient or the refusal's own status line already says why. Drawing on a PDF page is
  `DRAW-ON-PDF-PAGES`.

## distribution

- **WIN-SSH-JUNCTION-UNTRUSTED** [session] On a Windows computer, a sign-in over ssh by key cannot follow the
  install's junctions (`repo\current`, `julia\current`): a listing through either answers "The path cannot be
  traversed because it contains an untrusted mount point", and so does a process created through WMI from that
  sign-in. The installer itself works over ssh (it stages, and the next start at the desk applies), but nothing started
  from ssh can run the launcher's paths, so a daemon a remote person needs on a computer with nobody signed in at its
  desk has no supported start. Seen 2026-11-14; the daemon was started by hand with `SOT_RESOURCE_ROOT` set to the
  version folder and that version's `scripts/sot-local-daemon.ps1`, created through WMI so it outlives the ssh session.
  Fix not decided: a start for a daemon with no desk session that resolves the version folder without the junction.
- **INSTALLER-NEWEST-TAKES-A-DRAFT** [session] On a pre-release install the prelude of `scripts/install.sh` and
  `scripts/install.ps1` (and the preludes of 0.6.6-rc9.14 on) lists the releases through GitHub's API: `newest_release`
  takes the first entry, and from 0.6.6-rc9.15 on sends `GITHUB_TOKEN` or `GH_TOKEN` when set. Seen in test runs, three
  ways: with a token GitHub refuses, a 0.6.6-rc9.15 install's entry ends `FAIL: target: could not list the releases of
  kalidke/ship-of-tools` (401), where the same run without the token moves the computer to 0.7.0; on a hosted macOS
  runner, main's entry from 0.6.6-rc9.15 ended `FAIL: target: could not list the releases ... GitHub refused the
  anonymous request (HTTP 403, its rate limit for this network address)`, twice, as a computer behind a shared address
  would; and with a token that has push access the list holds drafts and its first entry is one (seen: the draft
  `v0.7.1-rc.3`, where an anonymous call gave `v0.7.0`), whose files answer 404 by tag (that run was not made). It is
  also a second rule for one concept: the updater takes the highest published version, the prelude the first listed, and
  GitHub lists by creation. Fix: one owner; at the least the prelude skips drafts, lists anonymously after a refused
  token, and falls back to the latest release when the listing is refused. Until then `docs/UPDATE-AGENT.md` reads the
  newest release from the latest-release redirect (no API call) and runs that release's own installer with
  `SOT_INSTALL_TAG`, which skips the prelude, the one reader of either token.
- **UPDATE-FROM-06-UNTESTED** [note] The release run's update legs start from `v0.6.6` and from the release before. Run
  by hand on 2026-11-11 with `docs/UPDATE-AGENT.md`'s command (Linux test containers, hosted macOS and Windows runners):
  from 0.6.6-rc1, rc9.13 (Linux and macOS), rc9.14 (Windows), rc9.15, 0.6.6, a pinned 0.6.6 and 0.7.0, each ended `PASS`
  at 0.7.0 and a second run changed nothing. Also run, in Linux test containers with a stand-in for the service manager
  and a stand-in agent in a real 0.6.6 row (so no row scope): the agent ran the command inside the row and was ended
  mid-run, the row started again with a new conversation, its repeat ended `PASS`, and a send to the row from a session
  outside any row said `filed`; with a live REPL in the row, the run without `SOT_INSTALL_TTY` hung
  (INSTALLER-ASKS-A-ROWS-TERMINAL), and with it the run stopped at the question, the yes ran the update, and the repeat
  ended `PASS`. Still in no gate, and not run: a row under a real systemd unit, a row on Windows or macOS, a window's
  lease, a Windows install older than rc9.14, a glibc 2.31 host, and `comm/tests/interop-066.sh`. Fix: a leg from the
  last 0.6.6 candidate, and part 1 of the interop script in the local gate while a 0.6 computer exists.
- **UPDATE-ORDER-UNRUN** [note] `docs/UPDATE-AGENT.md` step 2 ("Daemons before windows") and the check in step 5 were run
  in Linux test containers with a stand-in `ssh` and a stand-in service manager (several computers, each a scratch home
  with its own host name; a home shared by two computers, with a state folder of its own per computer), and their
  commands on hosted macOS and Windows runners against a 0.6.6 daemon on the same computer, read by 0.6.6's and by
  0.7.0's `sotd`. Not run: reading a far daemon through a real ssh login from any system (a slow login reads as unknown
  in step 2 and as not answering in step 5, which the runbook says to read twice), a real systemd unit, a home shared by
  computers that also share one daemon state folder (the daemon takes one lock per state folder, so the stand-in gave
  each computer its own), Windows or macOS updating by the runbook with older daemons, and a 0.6.6 install older than
  rc9.13, whose plan names `tcp:` addresses nothing listens on, so every daemon reads as unknown and the runbook asks
  the person (run on Linux with 0.6.6-rc1), and a Windows computer (the page counts every one as a window; the Linux
  rule that a computer's declaration and its install agree is read from the installer, not run). The stand-in `ssh`
  and `hostname` print each computer's test name, so the host-name check of a `--backend` pair ran against them, not a
  real login. A hub that
  is not the alias the sync named (a group member's copy naming a third computer) has an ssh check in the page that no case
  ran, and a failing `ssh ALIAS hostname` is shown, not tried. The runbook never updates another computer: it stops and names the order.
- **BACKEND-PAIR-NEEDS-HUB-HOSTS-TOML** [session] A `--backend <alias>` laptop installed before 0.6.6-rc9.13 (the window
  dials `SOT_HOST` through a tunnel) has no `hosts.toml`, and neither has its server. 0.7's installer on the laptop ends
  `FAIL: enroll: could not read the hosts.toml on '<alias>'`, and the 0.7 window dials only the plan's hosts, so even with
  both computers on 0.7 the window opens with no host until the server declares a group (nothing writes a first
  `hosts.toml`: `sotd topology set` edits one, `sync` fetches one). Seen in test containers (a 0.6.6-rc1 pair, both
  orders). The update runbook therefore stops on both computers of such a pair with nothing changed, and tells the person
  to write the three-block file on the server and run `sotd topology sync --hub <server>` on the laptop; with that file
  the same message moved the pair in a test. A server that holds a group file not listing the laptop is the hub owner's
  `sotd topology set add <laptop> <flags>`, the flag taken from the record (`--frontend` for a window computer, `--daemon` for a daemon-only member), and the
  runbook says so (tested for a `--backend` laptop, a daemon-only member and a `--local --hub` member). The group file's names are
  each computer's own host name and the window reaches the hub by that name in ssh, so a pair whose server prints another
  name than the alias the laptop uses (`lab` against `srv`) has no way through that the runs could show: the runbook
  leaves it as it is and says so. Fix: the installer on the server, or `sotd
  topology set add` on a hub with no file, writes the one-hub file, and the group file names a host apart from its ssh
  alias; a design for review.
- **AUTO-UPDATE-SPLITS-A-PAIR** [session] A release daemon checks for the newest release daily, stages it, and applies it
  at its next start; a window-only install updates itself at its next launch (`rust/frontend/src/selfupdate.rs`; this half is known by
  reading, only the daemon's staging was seen). Neither
  asks the other computers, so a restart of one computer's daemon or window can move it to a release its pair does not
  speak, with no agent involved. Seen in test containers: a 0.6.6 hub that had run for a few minutes had staged 0.7.0,
  and its next start applied it, with no agent having updated it. The runbook's stop tells the
  person not to restart the window or the daemon until the named computers are updated. Fix: the daemon applies a staged
  update only when no daemon it serves would be left on another protocol, or the staging stays off until the group is
  updated; a design for review.
- **UPDATE-OTHER-COMPUTERS-BY-SSH** [feature] When step 2 of the update runbook finds older daemons on other computers it
  stops and names them, and an agent on each computer runs the same runbook. Updating them from the one run, over ssh
  with each person's yes, is not built: it needs a proof per system (a Windows daemon over ssh has none) and ends the
  agents running on those computers.
- **INSTALLER-WINDOWS-ASK-UNWITNESSED** [note] Windows' `read_answer` (`rust/backend/src/install/daemon.rs`) asks on
  standard input only when it is a console, so an agent's run on a console-hosted Windows shell with its standard input
  at a pipe or empty should not ask; read, not run. Owed: a witness that such a run ends at the restart question with
  its `daemon:` line and changes nothing. `docs/UPDATE-AGENT.md` gives the installer an empty standard input there.
- **ROLLBACK-ACROSS-A-LINE** [note] The installer's rollback (`sot-apply.sh --rollback`) restores binaries, links, unit,
  wrapper and record, not comm and not rows. From 0.7 back to 0.6.6 the comm wrappers call `sotd comm`, which 0.6.6
  refuses, until the next launch; rows 0.7 retired stay ended, and 0.7 supervisors run on unseen by the 0.6.6 daemon,
  so a start there could run a second agent in the folder. Fix: refuse a rollback across a protocol change, or say in
  the manual that the move to 0.7 is one-way.
- **UPDATER-LISTS-TWENTY** [note] The updater reads one page of 20 releases (`rust/updater/src/fetch/mod.rs`). A stable
  install is offered nothing once 20 pre-releases are published after the newest stable; it holds today because
  candidates stay drafts. Fix: read GitHub's latest release for the stable channel if candidates are ever published.
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
- **SCRATCH-GUARD-RMDIR-STOP** [note] The scratch guard's run removal (`scripts/tests/scratch-guard.ps1`) is
  `cmd /c rmdir ... 2>&1 | Out-Null`; under a suite's `$ErrorActionPreference = 'Stop'`, Windows PowerShell 5.1 turns a native
  command's stderr into a terminating error (`scripts/tests/test-native-stderr.ps1`), so if rmdir printed anything the
  parent would end with exit 1. Found reading code in the scratch guard diagnosis; never seen.
- **HOSTED-RENDERER-APT-STALL** [note] The per-landing hosted run's step that installs the software renderer and the
  virtual display (`.github/workflows/rust.yml`, Linux shard `rest-0`) downloads its packages from Ubuntu's mirror under
  a 120 second bound; on 2026-11-10 the mirror served one 17.5 MB package too slowly, the step timed out (exit 124)
  before any build or test, and a re-run of the job passed; the same stall ended a second run that morning. A cached
  package set, or the bound widened only for the download, would keep a mirror stall from reading as a red run.

## chat

- **CHAT-DELIVERY-STALL-UNTOLD** [note] A stalled delivery shows only in the daemon log while `chat.resolve` says
  `delivers: true`: a channel whose state file cannot be read (it stays off), a start read of the inboxes that fails on
  every try (an inbox that is a directory), an `ambiguous_row` or `no_row` that refuses every filing, a chat held by
  another computer's daemon (status `Starting`) (`rust/backend/src/chat/watch.rs`, `chat.rs`). Fix: say it in the reply
  or the status. Found by reading code; never seen.
- **CHAT-PUBLISH-SKIPS-A-RESTART** [note] The daemon's chat owner task (`Own::sync`, `rust/backend/src/chat/watch.rs`)
  can take a new socket gap inside one channel's begin and then begin a later channel that has just left complete, in
  the same pass; `publish` then sees that channel pending, not needed, and sends no `chat.status`, so a window holding it
  complete is never told, and the daemon's new snapshot absorbs the gap's changes as its base. It needs two watched
  channels and a kick at the gap. Fix: `publish` also tells every channel that left complete since the last publish.
  Found reading code; never seen.
- **CHAT-TWO-READS-DISAGREE** [note] After a `complete` watch reply the window reads its own snapshot from Slack
  (`chat.history`), apart from the daemon's snapshot, so deletions between the two reads leave records in the window's
  page below the daemon's own set, held and never updated (the window owner's Limits, `rust/frontend/src/ui/CLAUDE.md`),
  and a read queued during the next connection's hello can answer an equal read on it. The shape that removes the class:
  the `complete` reply carries the daemon's latest page from its model, which deletes the window's overlay, stale flag,
  read and its matching, and the second Slack read per recovery. A wire and daemon change, for the maintainer's word.
  Found reading code; never seen.
- **CHAT-AFTER-HELLO-WAITS-CONNECTED** [note] The test rigs' `after_hello` waits for the socket's `connected`, which
  `rust/chat/src/socket.rs` sets one lock before it latches the hello's gap, so a test that watches right after it can
  begin before the gap and be left needing a snapshot. Every caller of `after_hello` shares the window. Fix: wait for
  the gap itself. Found by a review reading code; never seen in a run.
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
- **CHAT-GAP-CLOSING-UNTOLD** [note] The daemon tells a window of a socket gap only in a `chat.status` that lists channels
  owing a snapshot (`publish`, `rust/backend/src/chat/watch.rs`, which sends nothing when that set is unchanged or
  empty), and sends nothing when the gap closes, so no window can show the Slack link's state. Found reading code; never
  seen.
- **CHAT-DELETED-NOT-ALWAYS-DELETED** [note] The daemon's `chat.changed` `deleted` list is a before-and-after comparison
  of its model, not Slack's delete events (`rust/backend/src/chat/watch.rs`): when a snapshot is not installed it names
  every snapshot record its model lacks, and a trim handled with a delete can name the trimmed record. Since the window
  keeps a delete as `(message deleted)`, such a message, which still exists, shows as deleted until the next snapshot.
  Needs a rare race. Found by B5j's reviews, reading code; never seen.
- **CHAT-EMPTY-PAGE-NO-CURSOR** [note] The daemon drops joins, topic changes and other non-messages from a history
  page, so a run of 20 of them reads as an empty page that still says older history exists. The window then has no
  timestamp to page below and stops paging with `older messages could not be read`
  (`rust/frontend/src/ui/drawer/chat/owner.rs`). Paging past it needs the daemon to report the oldest timestamp it
  read (a field in wire's `ChatHistoryRes`). Found by B5n's review, reading code; never seen.
- **CHAT-MARK-SCROLL-SAME-FRAME** [note] Keys pressed before one frame apply the scroll first and the mark keys
  after, so PageUp then ArrowUp within one frame shows the mark where it was, one message older, and the view returns
  to it rather than staying where the page left it (`rust/frontend/src/ui/drawer/chat/view.rs`, `ChatView::layout`).
  Folding scroll and mark keys from one ordered list would fix it. Found by B5o's second review, reading code and a
  probe; never seen.
- **CHAT-MARK-ZERO-BODY** [note] With a body of zero height a mark key sets a mark while no record is placed, so
  "always in view" does not hold there (`ChatView::layout`). The compose rows likely leave at least one body row, so
  this is probably unreachable. Found by B5o's first review, by a probe; never seen.
- **CHAT-RERESOLVE-KEEPS-ANCHOR** [note] `ChatView::reset` runs only on a row change, so a row that resolves again
  to another channel keeps its position and mark by time against the other channel's records, which then pass on by
  the rule for a record no longer held (`rust/frontend/src/ui/drawer/chat/drawer.rs`, `ChatDrawer::tick`). It predates
  B5o for the position. Found by B5o's first review, reading code; never seen.
- **CHAT-WITNESS-KEYS-COPIED** [note] The native witness's keys go through `witness_key`, which copies the Chat branch
  of `drawer_pane_key`, `chat_key` and the Alt+Up branch of `scroll_line_chords`, because a winit `KeyEvent` cannot be
  built outside the event loop; breaking any of the three leaves the witness green
  (`rust/frontend/src/ui/drawer/chat/native_fixture_tests.rs`). A key press that carries its data instead of a
  `KeyEvent`, as the reply bar's key does, would let the witness call them. Found by B5r's review; never seen.
- **CHAT-THREAD-REREAD-CLEARS** [note] A thread read again (after the link returns, and after the person's own post
  in the open thread) clears its replies and shows the parent alone until the first page returns, so a
  reader inside the thread loses their place (`rust/frontend/src/ui/drawer/chat/thread.rs`). Merging the new walk into
  the held replies by time, and dropping the ones it did not return at its end, would keep them. A limit taken in
  B5p; never seen.
- **CHAT-THREAD-RETRY-WHILE-DOWN** [note] A refused thread read is asked again at its pace even while its link is
  down, so one read goes to a closed connection before the walk restarts at `Connected`; and a refusal for good
  (`stale_row`, `unmapped`) is asked again every 5 seconds where the channel's owner waits for a row or link change.
  Both are the hub's own refusals and cost no Slack read (`rust/frontend/src/ui/drawer/chat/thread.rs`). Found by B5p's
  stack review, reading code and a probe; never seen.

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

- **CHAT-RESOLVE-NAME-TEST-RACE** (chat) [note] closed by the landing that fixes it, 2026-11-14, seen in a hosted run (Rust run 43633631706, ubuntu): `a_resolve_carries_the_name_the_start_found_and_none_before_it` failed its count of `conversations.info` calls (2 against 1). The test waited only for the first configured channel's name, while the start's lookup goes on to the second (the rig maps two) and its call can land between the count's two readings. The test now waits until both names are cached, asserts the lookup's two calls, and counts after that.
- **SCRATCH-GUARD-TOKEN-LIFETIME** (distribution) closed by the landing that fixes it, 2026-11-14, seen in a hosted run: on main 82d8f1b97 the Windows job's "Test scratch guard" step passed every check until one threw `could not read the account's profile folder from its token`, and the same step passed on the three commits before. The cause is inferred, not reproduced: `Get-SotRealProfile` read `.Token` from a `WindowsIdentity` temporary with no later reference, so a garbage collection between the read and `GetUserProfileDirectoryW` could finalize the identity and close the handle. The identity is now held in a variable and disposed after the lookup, and the thrown line carries the Win32 error. No red was made.
- **SVG-DRAW-SILENT** (fe-ui) closed by the landing that fixes it, 2026-11-14, seen in use by a person: `d` over a shown picture the daemon cannot compose (an SVG, a PDF page, a video poster) said `drawing: no image to draw on`, though a picture was shown. Drawing over such a picture is not built (`SVG-DRAW`); `d` over it now says `drawing: only image files can be drawn on; open a PNG of it, or press <canvas key> for a blank canvas` (`drawing_over_a_picture_that_is_not_an_image_file_names_the_two_ways_to_draw`); `no image to draw on` stays under the refused-hello overlay. Over an SVG's text `d` still says nothing (the `Image` scope is not live there; a route of about 10 lines exists and is left, see `SVG-DRAW`).
- **PANE-COPY-KEEPS-WRAPS** (fe-ui) closed by the landing that fixes it, 2026-11-13, seen in use by a person: a line Claude Code word-wrapped itself copied with its break and indent, so a pasted command arrived as two lines; the new Copy as one line (`agent.copy_line`, `Primary+Shift+l`) joins every break into one space, a word the program cut at the width whole, and the vt100 fork flags the row of a wide character that wraps; the default copy keeps a program's own line breaks, since the window cannot tell a word wrap from a real newline.
- **WAKE-EATS-UNSENT-PROMPT** (messaging) closed by the landing that fixes it, 2026-11-13, seen in use by a person: a wake typed into a row whose prompt held a person's unsent line, so the instruction never reached the session. The person's loss was never reproduced in its plain form (a line already in the box is refused); what a test found, and this landing closes, is a race: keys a person typed after the wake's last screen read and before its typing reached the row first, and the wake line was appended to them, because the wake took the row's pen only when it typed. That race fits the report better than any other path, but it is not shown to be the person's loss, and the 0.6.6 report (a typed answer to a permission question gone) is explained by it only if an empty answer field reads free, which is unproven. The wake now takes the pen with an empty write once its first frame reads free, before its hold, so a key typed after that takes the pen back and refuses the typing, and one typed before shows inside the hold (`comm_wake::keys_typed_between_the_wakes_last_read_and_its_typing_get_nothing_appended`). A window's key frame refused because a wake took the pen, before the window had read its demotion, is no longer dropped: the attach client takes the pen back and sends those bytes once (`fe_attach::a_window_key_refused_because_a_wake_took_the_pen_is_sent_once_after_taking_it_back`), and one refused again is said on the status line as not sent (`keys_the_row_refused_are_said_not_sent`). What stays owed: a key typed, or an input the window held while its row connected, between the wake's text and its Enter lands after the wake line and is sent with it (messaging contract, Waking, the stated limit); the one loss that was Claude Code's own input redraw with no mail in flight is attributed to Claude Code only by the old entry's own sentence, never shown; a next loss needs the Claude Code version, a screen capture and the daemon's `comm wake: typed into the row` lines for that minute together. A 0.6.6 daemon has the race and takes this only by an update.
- **CHAT-SECRETS-TWO-FORMS** (chat) closed by the landing that fixes it, 2026-11-12, seen by a session operating the product as a user: a live-test tool's template wrote `chat-secrets.toml` as `[[workspace]]` tables keyed `id` while the daemon reads three top-level keys (`workspace`, `bot_token`, `app_token`), and the daemon refused that form twice at the first start of the chat on the hub (2026-11-10). The daemon's form is now the one documented form, with a sample parsed by `the_reference_samples_are_accepted_by_the_parsers` (`docs/src/ref/config.md`), and a refusal names its file. The tool is outside this tree; the fallback to its own form at the daemon's path lives in another repository's test tool, which that repository owns.
- **CHAT-REFUSAL-NAMES-NO-FILE** (chat) closed by the landing that fixes it, 2026-11-12, seen by a session operating the product as a user: the `project chat` log line gave the same words for a refused `chat.toml` and a refused `chat-secrets.toml`, and for a key the file must not hold only "a chat file is not valid", so a person could not tell which file to fix; the line (`Status`'s display, `rust/backend/src/chat.rs`) now starts with the file's name, and `ConfigError::UnknownKey` names a key the file does not take when another table of the chat's files accepts it (the accepted-key lists `only` checks, `rust/chat/src/config.rs`) and never otherwise, so no value and no key of the person's own is quoted (guarantee 21; tests `a_refusal_names_the_file_it_came_from`, `a_secrets_refusal_names_the_key_the_file_must_not_hold_and_never_a_value`). A mapping's stray key is named without its ordinal.
- **PDF-PREVIEW-NEEDS-POPPLER** (sidecars) closed by the landing that fixes it, 2026-11-12, seen in use by a person: a PDF showed no page on a lab computer running rows, which had no poppler (Windows computers almost never do, and a person without sudo on Linux cannot install it), because the PDF plugin ran `pdftoppm` and `pdfinfo`, and the video plugin `ffmpeg`, from the kernel's search path. The plugins now run the computer's own poppler and ffmpeg when its search path has them, and otherwise Julia's copy (`Poppler_jll`, `FFMPEG_jll`), which the first preview that finds no tool fetches in a child process into the user's own depot, once per tool per computer, answering at once with a note (nothing is downloaded at install or at the kernel's instantiate, and no JLL is loaded into the kernel); a request's tools together end inside the daemon's 10 s wait with the plugin's own note, and a failed fetch's fixed words reach the next note and are tried again after ten minutes (`ConceptExplorerCore.find_tools`, `core/src/fetch_tool.jl`, guarantee 19).
- **INSTALL-PARTS-TESTED-TWICE** (agents) closed by the landing that fixes it, 2026-11-12, known by reading code: `test/install_tests.jl` held two count-free tests of the rule that an installed tool runs the parts it sources; the later testset ("an installed tool runs every part it sources, whatever their number", a five-part stand-in tool) is deleted. The older testset ("comm-lib.sh and sot-fe install as one file each...") proves the rule on the real loader and `sot-fe`, whatever parts they source (guarantee 13), and the agents testset keeps each part out of the installed files.
- **CHAT-DELETED-PARENT-THREAD-UNSERVED** (chat) closed by the landing that fixes it, 2026-11-12, known by reading code and never seen: the recovery pass read the thread of every parent with a `latest_reply` after the boundary, a deleted parent's tombstone included, and a thread Slack did not serve would have ended every pass on the channel `ReadFailed`; on a thread read, Slack's `thread_not_found` now reads as an empty thread, the walk goes on and `Passed.skipped` counts it (`rust/chat/src/traverse.rs`, guarantee 26; tests `a_thread_slack_says_is_not_found_reads_as_empty_and_is_counted`, `any_other_slack_error_on_a_thread_read_still_ends_the_pass`). What stays owed: the read-only probe of `conversations.replies` on a deleted parent, which says whether Slack serves such a thread, so whether the rule ever fires.
- **LEGACY-SKILL-LIST-BEHIND** (agents) closed by the landing that fixes it, 2026-11-12, known by reading: `comm/legacy-skills.txt` lacked the latest versions of `sot-install`, `sot-setup` and `sot-statusline-setup`; the list is regenerated by `scripts/legacy-skill-hashes.sh`. No install acted differently: all three are onboarding skills (`CLAUDE_GLOBAL_SKILLS`), which the retire step never reads, and `test/protect_tests.jl` holds the list current for every skill it does read.

- **CHAT-LIVE-RECORD-NO-NAME** (chat) closed by the landing that fixes it, 2026-11-12, known by reading code and never seen in use: the daemon filled a member's name only on the `chat.history` op, so a record on the watch's own `chat.changed` (an edit or a new message) had the event's profile name or none, and the window, which replaces a held record on a put, could blank a name a history read had filled; a put now takes the names the API's cache holds (`Api::cached_names`, the author's and the mentioned people's), with no request, and a name never looked up stays empty (an author) or has no entry (a mention).
- **SOT-INSTALL-SKILL-OWN-UPDATE** (agents) closed by the landing that fixes it, 2026-11-12, known by reading: the `sot-install` skill, installed into every computer's global skills folder, kept an update procedure of its own (the installed entry, no questions) beside `docs/UPDATE-AGENT.md`, so an agent that took the skill's route updated one computer and skipped the runbook's step 2; the skill's update sections are now one pointer to the runbook at its raw address on main, with the fresh install pointer, the comm-only refresh and the after-update notes kept; test `global skills: sot-install points an update at docs/UPDATE-AGENT.md and holds no installer command of its own` (`test/global_skills_tests.jl`). `comm/legacy-skills.txt` is unchanged on purpose: `_retire_placed_skills!` skips the four onboarding skills, so it never reads their versions. Copies placed before this landing keep the old text until the next install, and the runbook's first lines tell an agent not to use them.
- **INSTALLER-ASKS-A-ROWS-TERMINAL** (distribution) closed on Unix by the landing that fixes it, 2026-11-12, seen by a session operating the product as a user: `sotd install` asked "Restart the daemon? ... [y/N]" and the role question on `/dev/tty`, which inside a row is the row's terminal, so an agent's shell tool waited on a question no person saw; `SOT_INSTALL_TTY` names the file when set, and otherwise `console` (`rust/backend/src/install/daemon.rs`) gives `/dev/tty` only where standard output or error is a terminal, so a captured run has no console and ends at the restart question with its `daemon:` line or the role question's refusal; tests `the_questions_go_to_the_named_file_or_to_the_tty_a_person_watches` and `sot_install_tty_names_the_file_the_answer_is_read_from`. What stays owed: INSTALLER-WINDOWS-ASK-UNWITNESSED.
- **PREVIEW-NOT-FOLLOWING-ROW** (fe-ui) closed by the landing that fixes it, 2026-11-11, seen in use by a person: a row left before its preview came back kept its request as the target it showed, so no return asked again, and its pane kept another row's text; a restore keeps a target only when its bytes answered it or the daemon refused it, and a row with no bytes shows no text. The other reading of the report is PARKED-ROW-PREVIEW-STALE.
- **UPDATE-WINDOW-BEFORE-ITS-DAEMONS** (distribution) closed by this records commit, 2026-11-11, seen by a person using
  the runbook: it updated only the computer its agent ran on, so a window moved to 0.7.0 while the daemons it dials
  still ran 0.6.6, and the window refused them ("the backend there predates the link; update it") with none of their
  rows. Reproduced in Linux test containers (a window computer unable to use a 0.6 hub and server, in a group and in a
  pair). Step 2 now reads the newest release and every daemon of the group (`sotd topology plan`, `sotd status`) before
  step 3, stops with nothing changed and names the computers to update first when one is on another line or cannot be
  read, and step 5 reads them again before it asks for a window to start; a rerun of the message from each state the old
  text left, in a group, ends in a stop that changes nothing, a repair, or a usable pair. Not closed by it: a `--backend` pair
  with no `hosts.toml` (BACKEND-PAIR-NEEDS-HUB-HOSTS-TOML: the runbook stops on both computers), what was not run
  (UPDATE-ORDER-UNRUN) and the daemon's own staging of updates (AUTO-UPDATE-SPLITS-A-PAIR).
- **SVG-IMAGE-HREF-READS-LOCAL-FILES** (fe-ui) closed by the landing that fixes it, 2026-11-11, known by a review's probes of the SVG draw: an `<image href>` in an SVG went to usvg's default resolver, which read any path that exists on the window's computer, so a FIFO hung the window's thread and a local PNG was drawn into the picture of a file served by a daemon; reachable through a markdown figure (known by reading). `svg_tree`'s resolver (`embedded_images_only`) now draws an image only from embedded `data:` data; a path or URL reads nothing.
- **RETIRE-06-DELETION-STRANDS-ROWS** (rows) closed by this records commit, 2026-11-10, known by reading: the rows
  contract planned `spawn/retire_06.rs` "deleted in the release after 0.7.0", but a 0.6 computer updates straight to the
  newest release and never passes through 0.7.0, so the deletion would leave a 0.6 terminal and its agent alive beside
  a second agent; the contract now keeps it until the installer refuses an install over 0.6 below 0.7.0.
- **CHAT-START-NOT-LOGGED** (chat) closed by the landing that fixes it, 2026-11-10: a chat start that succeeded logged only `the chat service is starting`; the daemon's boot now starts the chat's owner when the service runs and logs `the chat service is running` once.
- **SLACK-REPLIES-CURSOR-DROPS** (chat) closed by the landing that fixes it, 2026-11-10, known from a live measurement: Slack's `conversations.replies` cursor under a lower bound (`oldest`) alone drops one reply at every page boundary and never returns it, and its last page can hold the parent only with `has_more: false`, so a walk by that cursor looks complete and is not; `Api::history` (`rust/chat/src/api.rs`) now refuses such a continuation as `bad_request`, unsent, so no caller can reach it (the form's first page, and a continuation under an upper bound or no bound, are sent as before), and delivery's reads use `Api::scan`, which never sends a cursor (rule in `rust/chat/CLAUDE.md`, guarantee 11).
- **CHAT-NAME-ASKED-ONCE** (fe-ui) closed by the landing that fixes it, 2026-11-10: Enter in the Chat drawer with a draft and no `[chat] name` refused to post and sent the person to a hand edit of `settings.toml` and a restart; the drawer's foot now asks for the name once (`Name for your chat posts:`), checks it (`check_chat_name`), writes it durably into the settings file the window loaded, or the per-user one, and uses it at once, with the draft kept.
- **WINDOWS-SETTINGS-NO-DURABLE-HOME** (fe-ui) closed by the landing that fixes it, 2026-11-10: a Windows window read no settings file (the launcher's `SOT_SETTINGS` default named a file inside the release checkout, which an update deletes, and `$HOME` is unset there), so Enter in the Chat drawer never posted; the window's per-user place is now platform's config folder (`sot_config_dir_in`) for `settings.toml` and `keybindings.toml`, the launcher sets no `SOT_SETTINGS`, and the window ignores the daemon's `[trust]` section.
- **PREVIEW-PDF-STALE** (fe-ui) closed by the landing that fixes it, 2026-11-10: a PDF an agent rebuilt and badged as
  `show-result "$PWD/../<file>"` from its build folder kept showing the old version; the window took the path as sent,
  the daemon refused its `..` id, and the failure left the pane on what it showed, so each return to the row (the
  result never acknowledged) showed the old PDF again; the window now takes an agent's path in the tree's spelling, and a refused preview marks the pane's title (and the mark parks with the row's older bytes), and a reply for a badged file's newer re-fetch acknowledges its result.
- **TESTS-REACH-LIVE-COMM-HOME** (messaging) closed by the landing that fixes it, 2026-11-10: four destroy tests set no `SOT_COMM_HOME`, so a test run started from a session pruned, and took the registry lock of, that session's live comm folder; under test the one resolver now refuses a comm folder not beneath the temporary folder, so a test that forgets aborts the run with its name; every fixture that inherited the process environment's folder (the destroy, reauth, create-rollback and agent-environment fixtures) is isolated, and three argv tests now read no environment; a second landing isolated the four tests that resolved the folder through `HOME` or `USERPROFILE` with `SOT_COMM_HOME` unset (the gate's and the hosted runners' setting), one on every platform and three Windows argv tests.
- **GATE-SCOPED-MISSES-SHARED-CALLEE** (distribution) closed as not a gate defect, 2026-11-10: the scoped gate ran the
  same daemon test job, filters and home as the unscoped one; the test it passed was a race (COMM-GUARD-RACES-ENV).
- **BENEATH-TWO-WRITERS-WINDOWS** (platform) closed by the landing that fixes it, 2026-11-10: on Windows a write beneath a root kept its renamed file's handle, which shares no deletion, open through the folder's sync, so a second writer's rename over the same name was refused (access denied) for as long as that sync ran and failed once it outlasted the retry window, as `two_writers_of_one_name_each_publish_a_whole_file` did on a hosted run; the handle now closes at the rename, before the folder's sync.
- **CHAT-ROW-SWITCH-LAGS** (fe-ui) closed by the landing that fixes it, 2026-11-10: the Chat drawer lagged a row switch because a stopped watch's records were dropped and a first watch answered `pending` was asked again only after 1 s and at the next idle frame; the chat owner now parks the last two stopped watches that hold records and draws them at a switch back while the channel is read again, and asks a pending watch again at 250 ms and at 1 s, the idle loop sleeping to each ask. Left: a first show still waits for the hub's snapshot and the window's own read, and held records older than the snapshot's oldest are as read, so an edit, a delete or a reply count of one made while its channel was parked does not reach the window (rules and tests in `rust/frontend/src/ui/CLAUDE.md`, "Chat owner").
- **CHAT-GAP-ASKS-DEAD-TARGET** (fe-ui) closed by the landing that fixes it, 2026-11-09: the chat owner keeps the daemon's refusal in its gap (`Gap(Some(code))`), asks `stale_row`, `stale_mapping` and `unmapped` no more until a row or link change, and shows a gap, for refused snapshot reads too with the read's code, only after the daemon's slowest snapshot; `too_many_watches` and `unavailable` keep the once-a-minute ask on purpose, since each can clear.
- **MONITOR-TENANT-REACHES-REPL** (fe-ui) closed by the landing that fixes it, 2026-11-09: `help::Pane::of_drawer` is the one exhaustive map from tenant to pane, and in a tenant other than the REPL (Terminal, Monitor, Help, Chat) typing, paste, the line-scroll chords and the wheel no longer reach the REPL; `help::DrawerInput::of` is the one decision their handlers match.
- **GATE-LEFTOVER-ESCAPED-HOLDER** (containment) closed by the landing that fixes it, 2026-11-09: `output_does_not_wait_for_an_escaped_pipe_holder` (`rust/containment/src/signal_output_bound_tests.rs`) passed and left its escaped `sleep 30` running, so a whole local gate whose `sot_containment` job started within 30 s of the end failed its census (`leftover <pid> sleep 30`); the escapee now holds the pipe only while the test's `keep` file exists, and the test removes the file and sees the escapee gone.
- **REGISTRY-TEMPFAIL-LOCK-TURN** (messaging) closed by the landing that fixes it, 2026-11-10: the registry lock's turn is process-wide, so other tests' holds of it counted inside a test's 1 s bound (on a hosted Windows runner a write holds it about 0.3 s, and a panicking test holds it through its backtrace); every test that takes the lock, rows' included, now holds `registry::lock::TURN_TESTS`, alone when what it asserts rests on a bound under 10 s, and `TestHome`'s commands wait the default 10 s.
- **SELECT-MARKDOWN** (fe-ui) closed by the landing that fixes it, 2026-11-09, before its window test: a selection in a rendered markdown preview is shared as the whole file lines its text was rendered from, with the file's text there; a rendered buffer with no map shares nothing.
- **SELECTION-CODEX-HELD** (agents) closed by the landing that fixes it, 2026-11-09: `TAKES_SELECTION` leaves `codex` out, so a Codex row answers a selection `no_agent`, not `held`, until the Codex wiring is built.
- **SCRATCH-PAD** (fe-ui) closed by the landing that fixes it, 2026-11-09: a drawable preview pane whose drawing the row's agent sees; the window draws and shares it, the daemon writes its picture.
- **STATE-FIXTURES-RUN-ONLY-AT-RELEASE** (fe-ui) closed by the landing that fixes it, 2026-11-08: the window's 13 State fixtures were compiled by the local gate and the per-landing run but run by neither; the per-landing run now executes them alone on Linux (`child fixtures` of the `window_minimized` target, under `xvfb-run` and Mesa's software Vulkan driver), and the redraw mutation leg's comment in `rust/frontend/Cargo.toml` names `release-gate.yml`.
- **SCRATCH-GUARD-PROBE-SHARE** (distribution) closed by 366a34551, 2026-11-08: in a hosted Windows run on main the scratch guard test's concurrent run ended with exit 1 before its sleep; the run kept no stderr, and the cause is inferred: with the file held open as a poll read holds it, the old probe's `Add-Content` fails ("being used by another process"), which the old test shows under a forced overlap. Each probe now writes its file whole under another name and renames it into place; the daemon probe's later line is appended with `File.AppendAllText`; a failure of the concurrent run now prints that run's stderr.
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
- **SOTD-UNIT-RESTART-LOOP** (topology) closed by the landing that fixes it, 2026-11-13, seen by reading the code and a test, not in use: a `sotd.service` start refused by a daemon that answers on the socket or holds the daemon lock was restarted without limit (`Restart=on-failure`, `StartLimitIntervalSec=0`), every 2 s or every 152 s, and each 152 s try ran the relay refresh (reset-failed, a daemon-reload of the user manager, a restart of every relay socket) before it took the lock. Now the refresh starts only once the lock is held, and such a refusal exits `EXIT_SEAT_HELD` (69), which the unit lists in `RestartPreventExitStatus=`, so it rests until the next launch starts it.
