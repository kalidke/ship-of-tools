# Known defects and open items

The one list of known defects and open items, grouped by the subsystems `package.md` names. A pass reads its own
subsystem's part; no session loads this file at start, and no `@` include may name it. A session files an item
here, and a landing that fixes one changes it in the same commit: the item moves to Closed as one line.

## Form of an item

`- **ID** [mark] text`. The id is UPPER-KEBAB and never reused. The text says what was seen, where (file and function;
line numbers go stale), and the fix when one was decided; it holds no investigation log. A person's report is told in
reported form, with its date and the person's role, and never as quoted words (the root `CLAUDE.md`). The mark says how the item is known, as the root
`CLAUDE.md` defines a known defect:

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
- **BENEATH-TWO-WRITERS-WINDOWS-RECURS** [session] `two_writers_of_one_name_each_publish_a_whole_file` fails on about one
  hosted Windows run in 55. Six reds since the test was added (2026-11-08): a writer's `write_beneath` refused in four
  (main cf15468f1, run 42735292739, code 5; 90002ae5f, run 42799371530, code 5; 1a9f96c17, run 43306919232, code 5;
  5e2351570, run 43970451453, 2026-11-16, code 32, the rename now reporting the POSIX one's code) and the reader's open
  in two (5c2da1bac, run 43172516955; 7f364e24f, run 43888855282, both code 5; the second is
  WINDOWS-REPLACE-DELETE-PENDING). One writer failed each time and the other finished. Writer-side rate: 2 in 69 main
  runs before the handle closed at the rename (5f3e51af4), 2 in 111 since, so that fix changed nothing measurable.
  Hosted proof (2026-11-16, windows-latest, the platform test binary 300 times per branch, runs 43981545180 and
  43981549176, branches `platform/writers-proof-p0` and `-p1`): the turn closes one span. `a_writer_stopped_between_its_rename_and_its_close_does_not_fail_another`
  was red 300 times in 300 without the turn, each with `Err(Some(32))` for the second writer, and green 300 in 300 with it, so a writer's handle held on the final name
  does refuse another writer's rename for the whole retry window, and writers of one name in one process now take
  turns over it (platform guarantee 17; a writer in another process is not covered). The natural writer-side red did
  not occur: 0 in 300 without the turn (expected about 5 at the old rate), 0 in 300 with it, so what caused the hosted
  writer-side reds stays inferred (the other writer's handle on the name after its rename, for 250 ms or more) and the
  turn's effect on them is not shown. What the runs measured, each writer thread's longest over its 200 writes, across the
  300 runs: the span from a rename's return to its handle's close had a median of 3.6 ms (0.25 ms with the turn) and a
  longest of 193 ms without it (42 ms with it), below the 250 ms window; the longest rename call had a median of 20 ms
  without the turn and 27 ms with it, a 99th percentile of 128 and 333 ms, and a longest of 213 and 527 ms, so a single call can itself outlast
  the window. Defender ran in normal mode on the image. The reader side recurred: run 40 of 300 without the turn
  (and with the POSIX-first rename) failed a reader's read with code 5 (access denied), so
  WINDOWS-REPLACE-DELETE-PENDING's fix did not close it; none in 300 with the turn, which shows little (zero in
  300 happens 3 % of the time at the old rate). Reader side, from the logs and std's source (nothing ran on Windows
  for this): the red is one `std::fs::read` answering code 5, and no log says which of its calls (the open, or a read) or
  which NT status (delete pending, deleted and denied all read as 5); the test folded them into one error and the job kept
  three lines per panic (the test now names the call). Both writers finished their 200 writes in run 40, no writer was
  refused in any of the 600 proof runs, run 40 took 13.8 s (median 13.9) with its longest rename calls and
  rename-to-close spans at or below the medians, real-time protection off with `C:\` excluded (`WdFilter` and other
  filters still loaded). Reader-side rate per run of the test: 2 in 170 main runs with the classic replace first; with
  POSIX first, 1 in 300 on the branch without the turn and 1 in 600 on both; 3 in 770 all told (0.39 %, interval 0.08 %
  to 1.1 %); the two orders cannot be told apart (p = 0.30 and 0.12). Reader-side probe (2026-11-16, windows-latest,
  runs on `platform/reader-probe-p0`, nothing but counters and printing added): 300 natural runs, no refused read; by
  name, with four writers of 1,000 writes and four readers, 21.8 million reads, 9 refused in 400,000 replaces, and with
  two readers each in a process of its own, 31.7 million reads, 2 refused, so another process meets it as well. Every
  one of the 11: code 5 at the open (never at a read), NT status file deleted (`0xc0000123`), a rename in flight or just
  done, no fallback taken, and the open asked again at once answered in 40 microseconds to 2.5 ms. The writer's own
  handle on the final name does not refuse a reader (a test at the rename's hold point is green). The classic replace
  alone under a reader: 0 refused reads in 13.7 million reads and 20,000 replaces, too few to say. The reader side is
  fixed for platform's own reads by asking the open again within `RETRY_DEADLINE_MS` only on a status file deleted or
  delete pending (guarantee 15, `open_regular`); the two-writers reader reads through it; another program's read
  (the hook's `cat`, std's `fs::read` in other subsystems) gets the refusal (READERS-OF-REPLACED-NAMES-WINDOWS).
  Hosted proof with the rule in place (2026-11-16, `platform/reader-probe-p1`): 300 natural runs green, the rule's three
  tests green in all 300; by name, 11.7 million reads under four readers with 400,000 replaces, and 14.4 million under
  readers in other processes, no code-5 refusal got through, and the rule asked 10 and 7 opens again. A second state
  was seen, once in 100 stress runs, with the rule in place and not covered by it: 3 reads in one run, at the same
  moment, refused at the open with code 2 (not found), NT status `0xc0000034` (object name not found), a rename in
  flight (renames done 35 to 36), asked again about 7,800 to 8,000 times by the probe and answered after about 123 ms.
  A missing name is answered at once (guarantee 15), so the rule does not wait for it; the cause is unknown (the name
  is missing for as long as one rename call plus the probe's tight asking, which is far longer than the 40 microseconds
  to 2.5 ms of the first state). This item stays open for that reader-side state and for the writer side's unproven
  cause.
- **READERS-OF-REPLACED-NAMES-WINDOWS** [note] From reading code, never seen in use: on Windows a reader's open of a name
  at the instant it is replaced can be answered access denied (BENEATH-TWO-WRITERS-WINDOWS-RECURS measured it, in the
  reader's process and in another); platform's `open_regular` and `read_regular` ask again, a reader that calls std does
  not. Readers that can overlap a replace: the agent's `sot-selection-hook.sh` (`cat`; a refusal prints nothing, so a
  prompt goes without the shared selection, silently); `files/io.rs`, `files/preview/mod.rs` (one failed read, the next is
  right); the Julia kernel's and the agent's own reads of a saved file. Replaces that go through std's rename, with no
  retry and the classic replace first, and readers resting on "old file or new one": `comm/registry/registry.rs`,
  `comm/identity/self_file.rs`, `comm/mail/cursor.rs`, `comm/mail/inbox.rs`, `topology/store.rs`,
  `topology/relay_units.rs`, `agents/folder_trust.rs`, `install/record.rs` (its comment says "a reader sees the old file
  or the new one"), `files/preview/scale.rs`, `daemon_log/mod.rs` (all under `rust/backend/src`),
  `frontend/src/net/state.rs`, `frontend/src/ui/control/file_channel.rs`, `frontend/src/ui/persist/resume.rs`,
  `updater/src/manifest.rs`, `updater/src/pending.rs`, `updater/src/prepare.rs`. Two reactions are harsh: `rows/store/mod.rs`
  reads any read error as an empty file and the write that follows drops the sections it means to keep, and
  `frontend/src/net/state.rs` reads any error as no session file. Handled at each owner's pass: read through
  `read_regular`, or say the limit.
- **WRITE-DURABLE-TWO-WRITERS-UNIX** [note] From reading code, never run: two concurrent `write_durable` calls on one
  record use the same `<name>.tmp`. On Unix the second unlinks the first's temporary file and creates its own, and the
  first's `renameat` by name in `beneath_unix.rs` `publish` then publishes the second's unfinished file and answers
  `Ok`; on Windows the second fails at once on the first's share-read-alone handle. Guarantee 2 promises nothing for two
  writers of one record, so each caller rests on one writer per record at a time.

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

- **RECENT-ORDER-LEAVES-SESSIONS-WITHOUT-AN-AGENT** [person] The maintainer reported on 2026-11-22 that in the session
  strip's recent order a session with no agent never moved and sorted last, and that a new session started last. Cause:
  the order sorted by the work-state stamp alone (`activity_order`, `rust/frontend/src/ui/session/row_presentation.rs`),
  which a row whose session never joins does not have (`work_state::shown_of`). Built: rows says each row's last
  activity in the listing (`Workspace::active_at`, `workspace.list`'s `active_at`: the newest of the row's creation
  and its work-state stamp) and the recent order sorts by it, so a new session sorts first at its first listing (second
  when another session stamped in its creation second or later, or when the session selected in the window holds the
  first slot, which it keeps) and a session with no agent is placed by its creation; the default row keeps its
  creation across daemon starts, so one with an agent does not read as new after each; a window on a daemon older
  than this, and an older window, sort as before. Open: input typed into a session's terminal is not counted, so a session with no agent still never moves
  after its creation, until the capsule reports the time its terminal last took input and rows counts it; and no person
  has used the built part.
- **JQ-16-FLOOR-CHECKED-BY-NAMES-ONLY** [note] Review probes found jq programs that newer versions accept and 1.6 refuses. The gate uses newer jq and checks variable names in source. Hooks suppress jq errors, so a rejected effort filter reports unknown on a 1.6 host while the gate passes. This has not been seen in use. Run the hook suites under a pinned jq 1.6 as the gate does for Bash 3.2.
- **CODEX-STOP-LOSES-EARLIER-MARKER** [note] From reading `comm-status-idle.sh`: a Codex Stop reads the closing marker in its final-message payload. If unread mail holds that reply and the continuation omits the marker, the hook cannot recover it from the earlier Codex reply because the transcript slice for markers runs only on the Claude path. This has not been observed in a live row.
- **BOOT-STARTS-EVERY-ROW-AT-ONCE** [session] After a computer's boot the daemon started all of its 29 rows at once
  (2026-11-17, three boots of one hub, 33 to 37 supervisor launches within 4 to 7 s each time). On a computer with 32
  threads the load average was reported near 340 for about five minutes (reported, not measured); `workspace.list` replies took up to 49 s; 85 of the
  96 tool servers the agents launch through npm missed their 30 s connect bound (1.5 to 3.2 s on a quiet start); the
  sessions joined about 4 minutes after their launch (6 to 7 s on a quiet start); and 25 of 29 session starts failed
  on the registry lock (SESSION-START-FAILS-ON-A-HELD-REGISTRY-LOCK, under messaging). Cause: the boot resume
  (`run/resume.rs`) held its bound of four spawns only until each supervisor first answered, tens of milliseconds, so
  nothing bounded the agents' starts. Built: the boot resume begins a row whose supervisor does not answer at least
  3 s after the last, in the window strip's default order, and lists a row waiting its turn as `queued` (rows'
  contract). Open until a boot of a hub is seen to start its rows spaced while the daemon answers.
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
- **TEST-REAPER-LIMITS** [note] The test reaper (`rust/backend/tests/support/reaper.rs`, closing TEST-CAPSULE-OUTLIVES-TEST)
  is Linux only: a killed test leaves any macOS capsule (`Env`'s `Drop` never swept one there) and a Windows capsule that
  broke away from the test's job. It runs inside the test's control group, so a kill of the whole group takes it too;
  that is how `user-manager-tests.sh` runs its tests (`in-container.sh --user-manager`, whose unit ends the group with
  SIGKILL while its rows are scoped outside it), so a test killed there still leaks its row's scope (read from the
  scripts, not observed). It does not end a daemon a suite starts outside `Env` and never tells it (the daemons of
  `live_socket`, `ping_reaper`, `hub_link`, `topology_set`, `active_frontend` and `window_lease`'s lock tests, each the
  test's own child), a birth in flight past the sweep's 2 s, or a process a row's agent started outside its process
  group. A forked child of a test that has not yet exec'd holds the pipe's writer and delays the cue. A daemon is told
  at its launch and known by its command line, which reads empty until its exec is over
  (REAPER-CHECK-READ-BEFORE-EXEC), so a test killed within that time of a launch leaves that daemon running. The fix
  would be a second ender in each place, or one ender run by every test (`Drop` closing the pipe and waiting for the
  reaper).

## capsule

- **PARKED-CLIENT-CUT-COUNT** [note] `fe_attach`'s parked-client test asserts "a parked client was cut" from the
  statuses its worker saw since the worker started, but the supervisor's first dial in its start tick can be Unproven
  (guarantee 4's limit), so on a runner with four CPUs the assertion fails about 5 runs in 100, at the base of the fix
  that closed PARKED-RESYNC-COUNTED and after it. A red with no use behind it. Fix: count the statuses from the parking.
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
- **VT-AUTOWRAP-OFF-IGNORED** [note] From reading code, never seen in use: the vt100 fork has no arm for autowrap off (`CSI ? 7 l` in `decset`/`decrst`, `rust/vt100/src/screen.rs`), which Claude Code sets around a character that may not fit at the right edge. A program that relies on it can show a wrapped character where a terminal would overwrite the last cell.
- **WIN-VIEWER-TRAILS-ANOTHER** [note] From one hosted Windows run, not seen in use and not measured: one viewer of a row had not received the run's last bytes a second or more after another viewer had them (WIN-ATTACH-TEST-LINES-VIEWERS-UP-FROM-THEIR-ENDS). The leading guess is delivery lag on the Windows row socket, where each frame waits on a flush until its client has read it; a late resync or a loss are ranked behind it and no code path loses output without a resync. If it is lag, a window attached to a row on a Windows computer can show its output a second or more behind another window on the same row, then catch up with the same bytes. The mid-stream attach test now prints each viewer's delay to an end marker, which a hosted job shows only when the test fails or runs uncaptured (it passed on hosted Windows at its landing, so no delay was read): measure it with an uncaptured run before deciding whether this is worth a fix.

## messaging

- **SESSION-START-FAILS-ON-A-HELD-REGISTRY-LOCK** [session] Session starts (`comm-session-start.sh`) failed with
  `registry lock ... is held by ... (21s old ...): it is running` and `identity=FAIL`, in 25 of 29 rows, some twice
  before a later try joined (2026-11-17, in the minutes after a boot, while every row started at once:
  BOOT-STARTS-EVERY-ROW-AT-ONCE, under rows). Each holder was one live process that kept the lock 18 to 28 s (the lock
  file's age when a waiter gave up), longer by itself than a join's `SOT_LOCK_WAIT_SECS`, 10 s (`comm/PROTOCOL.md`,
  Bounds), and holders followed one another (seven between 1 and 3 minutes after the daemon's start; a waiter that
  outlasted one could lose the next take, ages of 0 and 1 s). A session whose start fails has no handle and reads no
  mail until someone runs the start again. Built: a full session start waits up to 40 s for each take (messaging's
  contract). Open until a boot of a hub is seen with every session start joined.
- **REGISTRY-LOCK-TESTS-UNDER-LOAD** [session] Two registry lock tests failed together in one local gate run on a shared
  Linux server (2026-11-16), at a commit that touched no messaging code, and passed when the gate ran again on the same
  commit: `a_marker_this_process_abandoned_is_adopted_by_its_next_writer` (`comm/registry/lock_tests.rs:181`, the
  reclaim marker not found, so the dead holder's lock was not reclaimed) and then
  `a_zero_wait_whose_retake_fails_makes_one_step` (`:431`, the error said the record changed during the reclaim). The
  same test process logged that the daemon's terminal child had no thread to run on (os error 11). Inferred, not
  shown: the first test's dead holder was not judged dead under a resource the test does not own (threads or process
  ids at that moment), and the second failed only because the first panicked with its `FAIL_REREAD` seam still set
  (a seam a panic leaves set reaches the next test that takes the turn; that part is read from the code).
- **WAKE-MISSED-FULLSCREEN-ROWS** [session] Three Claude rows still on Claude Code's alternate screen, their panes blank
  after the person switched his renderer setting, took no turn on mail filed to them (2026-11-15): their transcripts
  showed no `comm-poll` for over 40 minutes, while a line typed into each pane by a session (`[sot-comm] you have mail:
  run comm-poll.sh`, the wake's own words) started a turn at once. Whether the daemon typed no wake for them (each row's
  work state was blocked) or typed one the blank program dropped is not known; read the wake's log for those rows first.
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

- **CODEX-ROOT-THREAD-HOOKS** [note] The row binding uses the root `session_id` from Codex hook inputs.
  After release, check the live events and the bound id on a fresh start, resume, new conversation, clear,
  compaction, fork and child agent. Record which switches emit a start hook, whether the first prompt corrects
  the id after each switch and whether a child hook retains the parent's id. After clear, compaction and resume,
  verify that the session names its handle and handoff once, the open question stays stamped, and no second join occurs.

- **CODEX-SELECTION-WINDOW** [note] A Linux stub verifies the selection hook, marked PNG and withdrawal across Codex launches.
  To close, select plain text and rendered Markdown in fresh and resumed Codex rows in a real window. At the next
  prompt verify the notice, file, inclusive source lines and the full 2,000 UTF-16 units. Switch rows, then clear
  the selection and check subsequent prompts in each row and an ordinary session for delivery to the intended row only.
  Draw a distinctive mark over a figure and a different mark on the blank canvas. At the next prompt for each,
  check that Codex opens the named PNG and describes the mark. Replace the drawing with text, clear it, and check
  that the old picture is absent from the next prompt.
  After release, start a row with `--no-daemon` while a shared server is running. Check that the row resumes an
  earlier conversation, that `/resume` lists the project's conversations, and that the session otherwise behaves as before.

- **CODEX-ROW-SKILL-DISCOVERY** [note] The prompt renderer and launcher argv cover fresh processes. After release,
  compare the skill inventory of a resumed row running with `--no-daemon` with that of a plain Codex session.
  In the live row, ask in ordinary words to create a project log and check that Codex selects the
  plugin skill and reads its bundled resources.

- **CODEX-ROW-NO-SCROLLBACK** [note] `ccx` passes `--no-alt-screen` on fresh and resumed Codex launches, as verified
  with a stub. Keep open until a person uses a Codex row with this change and checks its scrollback, the view after
  reattachment and resizing, and the composer and menus.
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
- **CODEX-GLOBAL-SKILLS** [note] The six shared row skills now live in a cache-only plugin enabled by `ccx`.
  The three messaging skills and the work-state plugin remain global. Their scope is still open.
- **CODEX-INSTALL-REENABLES-HOOKS** [note] An installer review in November 2026 found that
  `codex plugin add sot-comm@sot-local` re-enables a work-state plugin disabled in personal settings.
  The same temporary-home probe reproduced this on the release before the row skills plugin.
- **TRUST-WRITE-DROPS-SAVE** [note] The reverse of REAUTH-TRUST-LOST, by reading: `ensure_folder_trusted`
  (`agents/folder_trust.rs`) reads the account's `.claude.json`, inserts the entry and renames its copy over the file
  outside Claude Code's lock, so a Claude Code save that lands between that read and that rename is dropped (a counter, a
  project field, or an account field). Not seen; the window is a few milliseconds. The spawn now makes a second write (the layer's, agents guarantee 6), a second such window.
- **TRUST-RESTART-MISSES-ADDED-FOLDER** [note] By reading, with the dialog's text measured (LAYER-ADD-DIR-TRUST-STOP): the
  trust restart (`rows/run/trust_restart.rs`, through messaging's `startup_of` in `comm/wake/screen.rs`) recognizes only
  the row's own folder-trust dialog (`Quick safety check:` with `Yes, I trust this folder`), so a layer record lost to
  Claude Code's own save (REAUTH-TRUST-LOST) leaves the "Adding a new directory:" dialog (`Yes, add it`, footer `Enter to
  confirm · Esc to cancel`) to a person. Rule if seen: `startup_of` also reads that dialog as `TrustDialog` (the restart's
  scope test is still the row's root), with a real capture as its fixture; a messaging change.

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
- **FILES-BYTES-READER-PDF-AS-TEXT** [note] The bytes-level reader answers a `.pdf` with the file's own bytes as
  `text/plain; charset=utf-8` (`mime_for_path` has no arm for it), which the window only summarizes as "binary file".
  After PDF-ZOOM-PAGE-PAST-TEXT-CAP no path seen in use sends a PDF there, but a kernel reply that is unmatched or
  malformed, or a live kernel's wire error, or a bounded plugin's non-picture, non-text output past 2 MiB, still does (a
  bounded picture past 64 MiB is refused with `preview_too_large`, not sent there). A bounded-output type should answer a note there, as video does.

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

- **SELECTION-BAND-HIDES-ITS-TEXT** [note] Measured by a review, 2026-11-19, not seen in use by a person: the
  selection band the preview and the agent pane share (`Solid::SelectionBg`, (252, 240, 130) at alpha 140 of 255 over
  the surface) is nearly as light as the text over it, so selected text is hard to read: the preview's selected text
  reads 1.49 : 1 against the band and the agent pane's 1.27 : 1, blended in linear light as an sRGB surface blends it
  (the edit view's own selected text would read 1.43 : 1 on it, so the edit view has its own dimmer band). Remedies: a
  dimmer band, as the edit view's (`EDIT_SELECTION_BG_RGB`); or selected glyphs drawn in the surface's color. Not
  decided.
- **EDIT-KEY-RESHAPES-A-LONG-LINE** [note] Measured by a review, 2026-11-19, not seen in use by a person: in the
  preview's edit mode a key re-shapes the whole line it lands in (`Editor::reshape_lines`, `buffer::line_change`), so in
  a file of one long line (a minified JSON, a long log line) a key costs what the whole file's shaping does: 853 ms a
  typed letter on a 446 KiB line in a release build, as before EDIT-KEY-RESHAPES-WHOLE-FILE's fix. A fix would shape a
  part of a line, which cosmic-text's `BufferLine` (one shaping per line) does not offer; not decided.
- **REPL-DRAWER-NO-SELECTION-OR-COPY** [note] A person cannot select or copy text out of the REPL drawer: its mouse only
  focuses and scrolls, its keys type, submit and recall history, and no path reads its rows back. Found while building the
  drawer's wrap (REPL-DRAWER-CLIPS-LONG-LINES); never reported by a person. A copy would have to give a wrapped line whole,
  without the breaks the wrap added, as the agent pane's does since 0.7.1.
- **WIDTH-CRATE-TWO-VERSIONS** [note] The frontend measures text with `unicode-width` 0.1 (eleven files outside the REPL
  drawer: `agent_pane/screen.rs`, `app/dialog.rs`, `chrome/{nav_body,spill,status,view}.rs`, `drawer/chat/row_channel.rs`,
  `input/{help,layout_help}.rs`, `preview/notice.rs`, `render/cells.rs`), while ratatui's buffer draws with 0.2, and the
  lockfile holds both. They disagree for an emoji-presentation sequence (a heart with U+FE0F, a keycap): a person could see
  a cell lost or gained per such sequence where those panes size text to the pane, as the REPL drawer's wrap did until it
  measured through ratatui. Found by the review of REPL-DRAWER-CLIPS-LONG-LINES; never seen in those panes. Fix: one shared
  width function over ratatui's own, used by all of them, and the 0.1 dependency dropped.
- **REPL-DRAWER-NO-SYNTAX-COLORS** [feature] Julia 1.12 and later color typed code in the REPL by its syntax; the REPL
  drawer shows typed code in one color. Put to the maintainer on 2026-11-19 as a choice; no go-ahead yet.
- **PAGE-RERASTER-PAST-PLUGIN-CAP** [note] `maybe_reraster_page` (`view.rs`) records the zoom that was asked for, not the
  size that was served, so each zoom step past the PDF plugin's 4096 px cap on its bounded axis asks for and ships the
  same capped bitmap again (from the kernel's cache): transfer wasted, nothing broken. Its comment says the plugin caps
  the long side; it caps the bounded axis.
- **FRAME-BUDGET-FROM-END** [note] The frame pacer waits `FRAME_BUDGET` (8.333 ms) from the end of the last frame, not
  from its start (`app/frame.rs` records the time after present; `app/pacing.rs` waits from it), so a frame that takes
  10 ms is followed by the next about 18 ms after it began, and the window tops out near 77 frames a second. Read by two
  diagnoses of the agent pane's page (2026-11-15); no use has shown it on its own.
- **RELEASE-BUILD-SCROLL-FIXTURES** [note] Two native State fixtures, `a_preview_scroll_moves_over_frames_and_comes_to_rest`
  and `a_repl_wheel_tick_rolls_a_row_at_a_time`, failed 4 runs of 4 in a release build on a Linux host at the unchanged
  v0.7.1-rc.9 tree, and pass in the debug builds the gates run. Not chased; their timing may assume a debug build's pace.
- **PDF-TEXT-SELECTION** [feature] Text the person selects in a PDF preview is shared with the row's agent, as a text
  preview's selection is (`requirements.md`, "The agent sees what the user selects"). A PDF page is shown as a picture,
  so it needs the words and their positions per page from the PDF plugin, the window's mapping of a drag to them, and
  the selection's page and text on the existing share. The maintainer asked whether it is listed (2026-11-10), and
  asked for it for 0.7.3 (2026-11-19, to select text in a PDF in the preview pane), which is the go-ahead. Read from the
  system's poppler 22.02 on Linux that day: `pdftotext -bbox-layout` gives word boxes, no character boxes, in a
  reading order it guesses and gets wrong on some two-column pages; whether Julia's copy of poppler, which a Windows
  computer uses, holds `pdftotext` is unmeasured.
- **DRAW-ON-PDF-PAGES** [feature] The person's drawing, shared with the agent, on a PDF page as on a picture. The
  maintainer, 2026-11-11, after using the drawing on a picture, a blank canvas and a markdown file, wanted it on PDFs
  as well, but not if it delays 0.7.1 significantly. He asked for it for 0.7.3 (2026-11-19, to draw on a PDF in the
  preview pane), which is the go-ahead. A PDF page is not a drawing base today: the window would
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
- **KEYS-DROPPED-WHILE-A-ROW-ATTACHES** [person] Keys typed while a row's attach is pending were discarded (reported as
  intermittent freezes and discarded keys) during a load average of 110 on 32 cores. The key loss is fixed: keys typed while a row
  connects are held in a visible line on the pane, held per row with an 8 KiB bound, and sent only by a fresh Enter one
  second after the row's screen was presented, into the run it showed. A warm client the window had parked for a row
  it switched away from stopped reading once 4 MiB of the row's output waited unread, and the row cut it at its 5 s
  write bound; shown again, it replayed the stale output as attached, and a key typed then was lost as delivery
  unknown. Reproduced in a test against a real supervisor and fixed: a client off screen keeps reading and drops the
  output, and asks for the screen when shown. Not reproduced: the pane's "episode failure ... status=redialing"
  warning (a debug build drains the stale output before the warning shows); that this cut is its cause, and the timing of about
  100 ms after the switch, are inferred from release-build drain rates, and no person's window log was read, so
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
- **CHAT-NAME-UNSHOWN-FOR-PERSON** [note] The drawer asks once for `[chat] name` and sends it with every person's post, but a hub that posts with the person's own token shows the person's Slack name and sends the name nowhere. The prompt serves only an older hub. Fix when no hub is older: drop the prompt and send an empty name (the daemon checks a name only for a row's post). From reading code.
- **CHAT-LINKS-THRICE** [note] The Chat drawer's three owners each keep their own set of connected hosts
  (`ChatOwner`, `RowChannel` and `ChatPictures` in `drawer/chat/`), all fed by `ChatDrawer::on_connection`: one link fact
  held three times, and a later owner (opening an original, a person's file post) would add a fourth. The fix decided by
  the pictures owner's review: the drawer holds one set of up hosts and hands it to each owner's asks; each owner keeps
  its own `unsupported` set, which is per op. Left because the change runs through the owner's and the row channel's
  tests, past the 30 lines it was allowed.
- **CHAT-ARRIVAL-OPENS-THE-PICKER-BY-ITSELF** [note] From reading code, never seen in use: a full-height drawer that
  arrives with a person's step, while the focus is on a pane it covers and no pane is maximized, takes the focus
  (`enter_row_drawer`, `person_driven`, `full_leaves_drawn`), and the chat's frame opens an
  unmapped row's channel picker by itself while the drawer has the focus (`ChatDrawer::tick`, once per row), which asks
  the hub for its workspaces and then for one workspace's channels, a Slack `conversations.list`. So stepping into an
  unmapped row with a full chat drawer costs that listing without a key pressed in the chat. Fix not decided: open the
  picker by itself only when the focus reached the drawer by a key in it.
- **CHAT-DRAFT-HAS-NO-DISCARD** [note] From reading code and a reviewer's probe, never seen in use: a message typed in a
  session's drawer keeps the channel it was begun in whatever happens to the project's channel. A row shown again begins
  or extends its draft at its first frame, before the hub's answer, and if the answer names another channel, none or a
  refusal, Enter sends the draft to the old channel, where the hub refuses it (the status line says `not posted: ` and the
  code's words, for example `the session's project now has another channel`) and the draft stays. Selecting another
  session and this one again keeps it, and the chat has no key that discards a draft, so only Backspace over the whole text
  clears it. The window sends a post it already knows is stale: `ChatDrawer::send` could compare the draft's destination
  with the confirmed target and refuse with true words. Fix not decided; a discard key and that comparison are the two
  candidates.
- **PREVIEW-ONE-PIXEL-LAYOUT-UNDER-A-FULL-DRAWER** [note] From reading code, never seen in use: a full-height drawer
  covers the preview, so the preview's rect is empty, and `layout_markdown` lays the markdown buffer out at
  `(width - padding).max(1.0)`, 1 px wide (`preview/layout.rs`). Each change between a covered and an uncovered preview
  (a switch to or from a row whose drawer is full) therefore resizes the buffer, which shapes the file again at 1 px and
  again at the pane's width. Measured since (2026-11-21, a release build, a 62 KB markdown file): 6.4 ms to build the
  file at 1 px against 5.9 ms at the pane's width, and 0.23 ms for the resize after it, so it costs a person nothing
  they would see. Fix not decided: keep the last non-empty width while the pane is covered.
- **AGENT-PANE-CURSOR-AHEAD-OF-TEXT** [person] Seen in use by a person on a Linux window, 2026-11-19 (the maintainer reported that after a switch to a session, typing moved the cursor faster to the right than the text, a gap that grew, and that backspacing it all away and typing again worked; the sessions were also open in a window on another computer). Not reproduced. The window draws the pane's text and the row's terminal cursor from one grid at one cell width (`paint_terminal`, `render/cells.rs`), and a test of that path found every glyph on its cell and the cursor after the text at three scales, after a switch's checkpoint, restore and resize. Two windows on one row can disagree on the terminal's size until this window's first key takes the pen (capsule `attach.rs`; the contract does not say that a redial takes the pen), but by test that moves rows and clips the right edge, not a gap that grows a cell per key. Open guesses, in order: Claude Code's own cursor running ahead when typing begins over the dim suggested prompt an idle row shows (then it is not the window's to fix); the size disagreement; fonts on that computer. What would tell them apart: whether the cursor is whole cells past the text on the same row, less than that, or on another row.
- **PREVIEW-TABLES-FIRST-VIEW-SHAPES-ALL** [person] Left by the landing that closed PREVIEW-TABLES-REBUILT-AT-EVERY-ARRIVAL, from the same report: a first view of a table-heavy page still shapes every table, rows out of view included, in one frame on the UI thread, and every pane waits for it. Shaping costs 0.8 to 1.6 ms per 1,000 bytes of rendered table text in a release build on the second host for the pages measured (up to 4.5 ms for dense shapes), by how many words a byte holds: 217 ms for the 62 KB report of seven wide tables (140,198 bytes of table text), 135 ms for a synthetic page of 142,458 bytes, 175 to 182 ms for the native fixture's page (10 tables of repeated words, 227,640 bytes), 278 to 284 ms for a page of 230,684 bytes of ordinary words (several measurements; a reply's frame has been measured from 135 to 303 ms). Four paths pay it, each in one frame: the reply's own frame for a page that arrives as a reply (a row's first visit, a `preview` command, a file that changed while the row was away; text and tables together); the frame after the arrival for a page restored from parked bytes with no kept set (text first, tables a frame later); the frame after an uncover when the page or the text scale changed under the cover, or with no kept set after a picture or the editor (the uncover frame draws no tables); and the frame of a text-scale step, which shapes every table again (231 ms for the report, 175 to 182 ms for the fixture's page; the step back costs the same again unless the earlier size's set is kept). The reply's frame is not deferred, since a document that changes every few hundred ms would blank its tables for a frame at each change. Two fixes are named, neither built: shape only the tables in view (each natural width from the monospace advance, about 40 lines), and the cheap shaping mode (24 ms measured against 216, no font fallback, so box-drawing characters and cell text the monospace face lacks need a check on Windows and macOS first). Its test: a window fixture whose first frame after a reply holding such a page takes a bounded time with the tables in view drawn.
- **PREVIEW-TABLE-PADDING-AMPLIFIES** [note] From a probe, never seen in use: `build_table_block` pads every row to the widest cell, so the text a table renders to is not bounded by the markdown file's size, and `PREVIEW_TEXT_CAP` bounds only the file. A 28 KB file with one 3,000-character cell and 2,000 short rows renders 6 MB of table text (217 times the file), which shaped is about 3 GB (a byte of padding holds 520 resident bytes) and seconds of shaping on the UI thread. The kept sets are bounded (a set over `PARKED_TABLE_BYTES` is not kept), the shown set is not, and it was not before the kept sets. Fix not decided: a cap on a table's rendered size, past which the table is shown as its source text.
- **PREVIEW-DOCUMENT-RESHAPED-AT-EVERY-ARRIVAL** [note] Measured on release builds on the second host, never seen in use at this size: every return to a row renders its parked document again inside `switch_to_workspace` (`restore_workspace_ui`, `render_preview_source`), before the first frame, so every pane waits. For a document of the text cap's size, two measurements agreeing: about 250 to 330 ms for 500 KB of markdown prose, about 700 ms for 455 KB of plain text, about 980 ms for one 500 KB code fence; and for a PNG's decode and upload, 194 to 204 ms for 6000 x 4000. The parked row keeps the bytes, not the shaped document. Fix not decided: keep the shaped document with the parked row within a budget, as the tables' sets are now.

## distribution

- **COMM-REFUSAL-ENDS-INSTALL-MIDWAY** [note] A review probe on 2026-11-21 found that a personal file at a shared command path, or any file the comm install cannot update, ends `sotd install` after the installed version has switched and the old one has been pruned, before the daemon restarts and the install record is written. The installer reports the failure and a second run completes after the cause is removed. This has not been seen in use. Have the installer ask whether the comm install can complete before switching versions.
- **WIN-COMM-HOOK-HOME-SPACE** [note] From reading code, never seen in use: the two hooks the comm installer writes into
  a person's own Claude settings (`comm-status-idle.sh` on Stop, `comm-subagent-guard.sh` on PreToolUse) run the command
  `$HOME/.sot-comm/bin/<script>` (`_hook_command`, `src/claude_hooks.jl`), with `$HOME` unquoted, so a shell that runs
  it splits a home that holds a space (`/c/Users/First Last` in Git Bash) into two words and the hook fails. The Unix
  installer refuses such a home; Windows refuses none. Unmeasured: which shell Claude Code on Windows runs a hook
  command with. The layer's hooks are written by quoted absolute path and are not affected. Fix not decided: a quoted
  home, `"$HOME"/.sot-comm/bin/<script>`; the unquoted spelling is already in `_EARLIER_HOOK_COMMANDS`, which an install
  retires, and `_comm_keep_json` and `_add_comm_hook!`'s manual line put the command into JSON text unescaped.
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
- **WIN-INSTALL-LISTS-WHOLE-PROCESS-TABLE** [note] From reading code, never seen in use: the installer's process listing on Windows (`rust/backend/src/install/procs.rs`) asks for the whole process table through CIM, the query that ran past 30 s twice in a hosted test (WIN-CIM-LISTING-30S, closed for the test alone). An install on a loaded Windows computer could wait as long.
- **GATE-SCOPE-SHELL-READER-OF-RUST-FILE** [note] From reading code: `agents/tests/test-sot-fe-chat.sh` reads `rust/protocol/src/ops/told_words_cases.tsv`, and a scoped gate run of a change to that file alone selects the protocol crate and not the agents shell suite, so `sot-fe`'s words could drift from the file unseen until a whole run. The gate's stated limit (a shell test that reads another folder is added by hand); the fix is a row for it.
- **HOSTED-ONLY-GATE-SUITES** [note] From a review, the class LOCAL-GATE-OMITS-ITS-SCOPE-SUITE was one case of: eleven suites for the gate's own scripts run only in the hosted checks job (`test-rc-gate-basename.sh`, `test-rc-gate-logs.sh`, `test-rc-gate-installer.sh`, `test-rc-gate-target.sh`, `test-gate-target-lock.py` among them), so a change to `rc-gate.sh` can pass the local gate and turn the hosted run red. Not seen red. The fix for the class is one check in the scope suite: every hosted `scripts/tests/test-*` step is either run by the local gate or exempted with its reason.

## chat

- **CHAT-MENTION-NO-AUTOCOMPLETE** [feature] Seen in use by a person on the 0.7.3 candidate, 2026-11-20 (the maintainer reported of a mention in a chat post that it works and is good enough for now, but that the user has to remember the exact spelling and name used, with no autocomplete): the Chat drawer's draft offers no names while a person types after `@`, so a mention needs the member's name as Slack has it, and a misspelled one goes out as plain text and is told back as unlinked only after the post. Wanted: the workspace's members offered as the name is typed, from the names the daemon already resolves a post against. Unbuilt, and not in 0.7.3 on his word that the mention is good enough for now; it needs his go-ahead.
- **CHAT-MAY-EXIST-NAMES-NO-MENTIONS** [note] A post whose answer was lost (`chat.post` or `chat.post_file` answered `unknown`, "may exist") tells the poster no `mentioned` or `unlinked` names, though the daemon knows what it sent: the reply is wire's unit variant `Unknown` (`#[serde(other)]`, which carries no field). From the mentions landing's review, reading code; never seen. A person is sent to the channel, where the drawer shows the posted message with its mentions by name. The fix named: a new reply variant (for example `unknown_told`) carrying the optional, lenient `mentioned` and `unlinked`, which older readers already take as `unknown` through `#[serde(other)]` (an older `sot-fe` exits 3 on it); the daemon sends it for an `Unknown` outcome with names to tell, and fe-net's reply mapping, the owner's `PostReply` and `PostShown`, the status words and `sot-fe`'s case print the may-exist words and then the report, for a text and a file post: measured at about 35 product lines with its pairing test, over the 30 its landing allowed, so it is its own change.
- **CHAT-MEMBER-WALK-NEVER-COMPLETES** [note] A post's member list walk (`Api::roster`, `rust/chat/src/api.rs`) runs inside the post's 10 s names deadline and is dropped with it, so in a workspace whose ten `users.list` pages take more than 10 s no walk ever completes: every post that names someone waits 10 s, goes out with its names as text (`the member list could not be read` on the status line and in `sot-fe chat post`'s output), and spends up to ten tier-2 reads, a rate limit among which pauses every read of the workspace. From the review of the mentions landing, reading code; never seen. The fix decided: the walk runs to its end as its own task and fills the cache while the post waits for it at most the names deadline; it needs `Api` held in an `Arc` by the task and a rule that the task ends with its workspace's service, so it is its own change.
- **CHAT-STATUS-NAMES-NO-WORKSPACE** [note] `chat.status` names channels and no workspace, so a connection watching in
  two workspaces is told only by its own workspace's owner, and when one workspace stops it resyncs the channels of that
  workspace alone; a window watches one channel at a time, so it is never misled. From the multi-workspace plan's
  review, reading code; never seen.
- **CHAT-ROLLBACK-SEVERAL-WORKSPACES** [note] A daemon from before several workspaces reads `chat-secrets.toml` alone and
  refuses a `chat.toml` whose mapping names another workspace (`SecondWorkspace`), so a hub rolled back with several
  workspaces mapped has its chat off, with that line in its log, until the newer daemon returns; nothing is lost, and
  while one workspace is connected every file the daemon writes is one an older daemon loads. From the plan's review,
  reading code; never seen.
- **CHAT-OLDER-WINDOW-ONE-WORKSPACE** [note] A window that does not name a workspace (a window older than the one that
  lists them, `chat.workspaces`) lists channels and pairs in the workspace `chat.toml` names alone; after connecting a
  second workspace, its connected screen's Enter opens the first workspace's list. From the plan's review, reading code;
  never seen.
- **CHAT-CONNECT-ANSWERS-CONNECTED-WHATEVER-THE-RELOAD-DID** [note] `chat.connect` replies `{workspace}` once the tokens are
  saved and the reload's wait ended, and logs and discards the reload's status (`rust/backend/src/chat/setup.rs`), so a
  person who pastes another Slack app's token (or one Slack then refuses) sees the Connected screen, and the chat ends
  `wrong_app` or `tokens_refused` within moments after it. Since chat guarantee 34 a set of tokens Slack tells to be two
  apps' is refused at the connect and never reaches the reload; what stays is a set whose apps Slack did not tell and a
  token Slack refuses only after the connect. The reply is blind by design (`a_connect_whose_start_fails_is_saved_and_resolve_says_start_failed`);
  the window resolves the row again after a reply and again on a stopped status, which brings the Connect step, but the
  Connected screen has been shown first. From a review, reading code; never seen.
- **CHAT-START-REFUSAL-IS-START-FAILED** [note] A bot token Slack refuses at the daemon's start is `ServiceError::Api(Slack(_))`,
  whose code is `slack`, so the chat is `Failed("slack")` and `chat.resolve` says `start_failed` (the generic first
  screen), while the same refusal while running ends the service `bot_token_refused` and says `tokens_refused` (the
  refused screen); the wire's comment on `tokens_refused` ("by the load or by Slack") is true only of the second. From a
  review, reading code; never seen.
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
- **CHAT-START-FAILURE-FINAL** [note] A workspace's chat start that fails for a passing cause (no network at the daemon's
  start, Slack down) stays failed until a reload (any `chat.connect` or `chat.pair` that changes the file starts
  again every workspace that is not running) or a daemon restart: only a start refused `held` is tried again (`Chat::launch`, `rust/backend/src/chat.rs`).
  Each workspace fails alone; the others run. `chat.resolve` answers `unconfigured` (`start_failed`) for it, so a
  window can offer Connect Slack, which reloads. The fix if it is seen: a daemon retry of such a start on a pace. From
  reading code; never seen.
- **CHAT-PERSON-POST-NOT-ANSWERED** [person] A post from the Chat drawer reached Slack as the app's own message: a
  `chat.postMessage` made with the bot token under the custom author `<name> (person via Ship of Tools)` (`Api::post`,
  `rust/chat/src/api.rs`). Another responder in the channel, which skips every message with a `bot_id`, never answered it
  (seen by the maintainer, 2026-11-16, who had meant a post from the drawer as himself to be answered by the bot). Fixed in the
  tree, not yet checked in use: a person's post now goes out with the person's own Slack user token, carrying only the
  channel, the text and the thread; a hub with no such token, or whose token Slack refused, posts nothing for the person and
  the window's Connect step asks for a connect; a row's post stays the app's. What remains unmeasured is the premise the goal
  rests on: that Slack's message for a post made with a user token has no `bot_id` and no `subtype` (`rust/chat/CLAUDE.md`
  guarantee 2; every test reads the post back in the shape that premise assumes). Check at a person's first post on a release
  candidate, with a control (a message typed in Slack's own app is answered); close this item then, and if the responder
  still does not answer, keep it open with that evidence.
- **CHAT-DRAWER-ORIGIN-LEGACY** [note] Nothing makes a `Drawer` post since a person's post went out with their own
  token. `normalize`'s reading of an own post's `person` metadata, username suffix and file-share label, `mail`'s `the
  chat drawer` role and the window's `DRAWER` badge serve only posts made before. Deleting them changes the wire
  (`Origin::Drawer`, which an older reader takes as `other`), and older drawer posts would then read as the app's `Bot`
  and not as mail. From reading code.
- **CHAT-CONNECT-MAKES-A-NEW-APP** [person] For a workspace already connected whose secrets held no user token, the
  Connect step's post-as-you screen (`connect.rs`) said to click `Ship of Tools (<login>)` in the person's list of Slack
  apps and, if it was not there, to make the app again; it never named the app connected now, so a person whose connected
  app had another name made a new one, which was in no private channel (seen in use by the maintainer on v0.7.1-rc.13,
  2026-11-17: the project's private channel went unread and he chose another), though adding the user scopes to the
  connected app and installing it again keeps every channel it is in. Built, not yet checked in use: the service keeps the
  bot's app from `bots.info` at the start (`Service::app`), `chat.workspaces` carries its id and its bot's name
  (`app_id`, `app_name`), and the post-as-you screen says first to keep that app, with the name Slack calls it, and
  Enter opens its own page (`https://api.slack.com/apps/<app_id>/oauth`); before the hub's answer, and from a hub that
  gives none, Enter opens the list of apps and the screen says to look there for the Slack app connected before,
  whatever its name, and to make a new one only if there is none, since a new app is in none of the private channels. Also
  built, not yet checked in use (the design's second form): a connect that does not say `replace_app` writes only when
  no chat of its workspace runs or starts, or the running app is told and is the tokens' app; any other writes nothing,
  restarts nothing and is refused `other_app`, decided under the setup lock with a start waited for first and a running
  chat whose start could not tell its app asked for it once there, the answer kept (`rust/chat/CLAUDE.md` guarantee 31;
  before that ask, such a chat left every connect asked about for its whole life, with a screen that said to ask Slack
  again: from a review's probe, never seen by a person). The tokens' app is told by two answers on every connect, `bots.info` under the
  bot token and the first frame of one probe socket opened with the App-Level Token, closed again before anything is
  written; told to be two apps, the set is refused `mixed_apps`, with `replace_app` too and on a first connect
  (guarantee 34), so the daemon writes no set that would stop the chat `wrong_app` for every project of the workspace,
  by whatever keys it was pasted (from reviews' probes, never seen by a person: the new app's App-Level Token, still
  on the clipboard, pasted beside the connected app's two codes). The window's Connect step asks on one screen,
  `a different Slack app`: told to be another app's, Enter keeps the app connected now (its page opens, the pasted codes
  are dropped) and Right connects the pasted codes anyway (`replace_app`); not told, Enter asks Slack again with the
  same codes and Right connects them anyway. A mixed set returns to step 3 with the wire's words, the App-Level Token
  dropped and the other two kept. Not asked about: a chat that is not running or ended on its own. Written though Slack
  did not tell their apps: tokens on the person's Right, and tokens of a first connect (the listener's `wrong_app` is
  then the backstop); the user token's app is told by nothing. An older window cannot replace a running app
  at all (it reads the refusal as an unknown outcome and says Slack may be connected, which is not so). Open: five premises are unmeasured:
  that Slack's `hello` names the socket's app (`connection_info.app_id`), that an envelope a probe socket left
  unacknowledged is sent again to the app's other connection,
  that `bots.info`'s `bot.name` is the name Slack calls the app, that the `/oauth` address opens the app's page, and that
  `bots.info` under a new app's bot token names that app. Check them at a person's first use of either screen; close this
  item when a person keeps the connected app through one.
- **CHAT-STATUS-READS-STOPPING-AFTER-AN-END** [note] From a review, not seen in use: after a service ends on its own, `Chat::status()` reads `Stopping` until the stop it began returns, so two readers depend on timing: the owner waits a bounded time for `Off(Failed(code))` before it tells its watchers (CHAT-WATCHERS-TOLD-BEFORE-THE-CHAT-IS-OFF), and a watch refused by `Hub::ensure_owner` can make a window resolve the row while the chat still reads `Stopping`. Letting the status read `Failed(code)` from `service_ended` on, whatever the stop still does, would close both and delete the owner's wait.

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

- **REAPER-CHECK-READ-BEFORE-EXEC** (rows) [note] closed by the landing that fixes it, 2026-11-22, a red in a hosted run with no use behind it: the Linux `rest-0` shard of the Rust tests on main hit its 5-minute step bound with `support_checks::the_reaper_ends_only_a_process_that_names_this_envs_project_root` running, on a commit that changed no Rust. `Command::spawn` returns once the child's exec has released its parent, before the kernel has set up the child's arguments, so until the exec is over `/proc/<pid>/cmdline` reads as the parent's own or empty. The test spawned its stand-in daemons and at once had `end_daemon_of` read their command lines; on such a read the matching stand-in named no `--project-root`, so it was left alone with `Ok`, and the test's unbounded wait on it, a shell that loops for good, never returned. Reproduced in 44 of 300 runs on a 64-thread Linux computer, each hung run leaving the matching stand-in running, never signaled. Each stand-in, and the folder test's holder, now writes a line once its exec is over and the test reads it before any command line is read, and the wait on the matching stand-in is bounded (`support::BOUND`), so a reaper that leaves it alive fails with its answers; 2,000 of 2,000 runs pass after. The reaper's own read of a daemon told at its launch keeps the window (TEST-REAPER-LIMITS).
- **CHAT-PACE-TEST-READS-ARRIVAL-GAPS** (chat) [note] closed by the landing that fixes it, 2026-11-22. A hosted macOS run of the Rust tests failed once in `traverse_tests::a_pass_paces_its_reads`, on a commit that changed nothing in the chat: 46 ms between the third and fourth reads at a pace of 400 ms. The pace is between sends (`traverse.rs`, `Run::read`), and the test held each gap between arrivals at the fake to three quarters of the pace; an arrival late by the runner's scheduling shortens the gap to the next one, which the test does not own. The test now holds each arrival to its distance from the first (read `i` at least `i` paces less three quarters of one), which a late arrival can only lengthen, and prints every arrival when it fails. With the pace's wait removed from the product the test is red. Not shown: that the run's third arrival was late (the log has that one gap only); if the test fails again, its message has every arrival.
- **CODEX-STOP-HOLDS-SHORT-TURNS** (rows) [session] closed by the landing that fixes it, 2026-11-21. A session operating a Codex row saw the Stop hook hold two turns shorter than five minutes and report 9,999 tool calls over 166 minutes for each. The hook now measures the matching turn from its rollout with a jq 1.6-compatible filter. Either eight observed calls or five measured minutes qualifies. Unreadable call records and a full tail without a start yield labeled lower bounds. A full tail with only the other agent's record kinds or a conflicting Codex turn ID supplies no such bound. Pinned by the short-turn, threshold, partial-measurement, full-tail and jq keyword cases in `comm/tests/test-status-floor.sh`.
- **STUB-STEP-RUNS-SUITE-CLEANUP** (agents) [session] closed by the landing that fixes it, 2026-11-21, seen in one local gate run by the session that lands changes: the `sot-fe` chat suite passed its first two cases, then lost its whole work folder and failed every later case on a missing staged script; the same suite under bash 3.2 passed in that run, and a rerun passed. Six suites fork their stand-in daemon's steps (`nc`, then a watcher) under `set -m` while the suite's EXIT trap is set. A stop that reaches a just-forked step before bash resets the traps it copied runs that trap in the step, and the trap's cleanup removes the suite's work folder; measured on one computer, about half of subshells signaled at once ran the parent's EXIT trap, and none did when signaled 1 ms later. Now each of the six suites clears its EXIT trap on the `set -m` line and sets it again on the `set +m` line, and `comm/tests/test-rm-guard.sh` scans every suite that sets an EXIT trap for the pair (`trap_across_m`; red on the six before, with a synthetic offender and a clean case). The same harness closed its descriptor with `exec 3>&- 2>/dev/null`, which sends the suite's own stderr to /dev/null for the rest of the run (an `exec` of redirections alone is permanent), so the failed run showed no message from its later commands; the 12 such lines in the six suites are now `{ exec 3>&-; } 2>/dev/null`, and the scan's second rule (`exec_silences_suite`) refuses the form. Limit: the timing is reproduced only with a preloaded stall of the fork on one computer and is not a tracked test; `comm/tests/test-endpoint-gate.sh` is exempt from the scan because it stops its stubs only after they have shown they run their own code.
- **PREVIEW-TABLES-REBUILT-AT-EVERY-ARRIVAL** (fe-ui) [person] closed by the landing that fixes it, 2026-11-21, seen in use by a person on the 0.7.3 candidate: of one session among many, the window was slow to show it on every switch, and fast once another file had been put in that session's preview. A session had put a 62 KB markdown report of seven wide tables in the row's preview. The window kept one set of markdown table buffers and cleared it whenever markdown was not on screen, so every arrival at a session whose preview held large tables shaped every table of the file on the UI thread, in the one frame that draws every pane (216 to 223 ms for that report, a first frame of 263 to 265 ms against 1.1 to 1.6 ms for the frames after it), even while a full-height drawer covered the preview. Now sets are kept: a set the frame no longer shows is parked, newest first, within 200,000 bytes of rendered table text (the text the tables render to, box-drawing characters and padding included, not the markdown file's bytes). One file of the report's size (140,198 bytes of table text) stays warm beside the shown set, or any others totaling at most 59,802 bytes, and three such sessions stepped in turn are all cold; a document with more table text than the budget is never kept. A set is taken back only for the same table texts at the same font size and line height. A byte of rendered table text held 76 to 520 resident bytes in the measurements (the worst is padding), so the kept sets hold up to about 104 MB beside the shown set, which nothing bounds (PREVIEW-TABLE-PADDING-AMPLIFIES). A frame that shows no markdown drops the shown set (kept when within the budget); a covered preview (a full-height drawer, a maximized pane) builds nothing and keeps the set it holds and the tables' horizontal scroll, whatever its size. The first frame of an arriving row, and the first frame that shows tables again after a frame that showed none or covered them, shapes nothing when no set is kept: it draws every pane with the tables' places empty (the walk reserves each table's height from its line count, so nothing moves) and asks for the next frame, which shapes them. The font size is part of the key, so the tables now follow a text-scale step; before this landing the key ignored the metrics, so after a zoom the held buffers kept their old size and a step cost 2 to 5 ms, and now a step is one whole build (costs in PREVIEW-TABLES-FIRST-VIEW-SHAPES-ALL). Measured on release builds on the second host: on the report, the table stage of a return took 5 to 12 microseconds and its first frame 3.6 to 4.1 ms (the document's own text work, which the next frame does not pay), where the parent's first frame took 222.5 ms; on a synthetic page of seven tables (103,530 characters of table text) in the native fixture, a return 1.3 ms, an arrival or an uncover with none kept under 1.1 ms with no table drawn and the next frame 143 to 154 ms with all seven, and an uncover with the set kept 1.4 ms; on a synthetic page over the budget (227,640 bytes of table text), uncovering a full-height drawer 1.3 ms with all ten tables drawn, scrolled as they were. Left: shaping every table of a page that has no kept set is still one frame, and what a person sees depends on how the page came. What a person sees then, by path (a page restored with no kept set, a reply, an uncover after a change under the cover, a text-scale step), and the costs, are in PREVIEW-TABLES-FIRST-VIEW-SHAPES-ALL. Pinned by `a_set_left_is_taken_back_for_the_same_sources_and_metrics_only`, `the_sets_kept_stay_within_the_budget_oldest_first`, `a_set_is_charged_its_rendered_bytes_whatever_it_holds`, `the_real_budget_keeps_a_set_at_it_and_drops_one_past_it` and the native fixture `a_row_of_wide_tables_arrives_without_shaping_them_in_its_first_frame`.

- **CHAT-SWITCH-RELOAD-TAKES-A-SECOND** (fe-ui) [person] closed by the landing that fixes it, 2026-11-21, seen in use by a person on the 0.7.3 candidate (the maintainer reported that the Slack pane took a second longer to reload when switching session). Not a regression. For an arrival no other window holds, the window's first `chat.watch` is answered `pending` while the hub reads the channel's page from Slack (read from the daemon's code; the hub's log does not show that read, only the window's own `chat.history` reads, 238 to 306 ms in eight of ten (437 ms once on the first day, 1,507 ms once on the second, the first read after the hub's chat restarted), and, on the day he saw it, that the first of them started about 1.4 s after the switch). The window asked again at 250 ms and then only at 1 s, so when Slack took longer than about 250 ms the second ask lost the race and the window waited 750 ms more for a snapshot the hub had held since about 0.3 s after the first watch (inferred from the window's own read: the hub's snapshot read is never logged): the channel's messages were current about 1.7 s after the switch (1.4 s to the window's own read, 0.3 s for it). On the earlier day, with reads of 238 to 250 ms, two of four arrivals fit a read at the second ask (or a channel another window held) and two the third, as the timings read; the drawer was empty meanwhile except for the last two channels shown. Two landings. The first replaces the two paces by one that grows: an ask after a quarter of the recovery's age at the last one, at least 100 ms and at most 5 s, and no ask while the last one's reply is awaited (`ChatOwner`, `Watch::pace`), so a snapshot the hub holds N after the first watch is read within the largest of 100 ms, a quarter of N and one round trip of the link, to the 20 s the window gives it (a prediction of about 1.05 s from the switch instead of 1.7 s for a 300 ms read, not yet measured in use); it costs at most 12 asks in the first 2 s and 23 in all, the last at about 22 s (fewer on a slow link), where it was 3 in the first second and 7, and the instant the title first says `not up to date`, 26 s when the first admitting answer came before the old 6 s ask, is now 25 to 30 s by how the replies come. This one draws the row's last messages at the first frame: `RowChannel` remembers each recent row's last mapped answer (host and row id, at most `PARKED` rows), and until the row's resolve is answered the drawer draws the owner's parked records of that channel under `reading…`, sending nothing for it (the owner has no watch for it, and a post, a file's post and a picture's open are refused meanwhile, while typing is not held: a key begins the draft in the channel drawn); the owner parks eight channels, each its newest 100 records (at most 25 MiB, from chat's 32 KiB record bound), where it parked two of up to 256. Pinned by `a_row_switched_back_to_draws_its_last_records_on_the_first_frame`, `an_answer_that_is_not_the_remembered_channel_removes_the_drawn_records`, `the_remembered_answers_are_bounded_and_keyed_by_host_and_row`, `a_row_drawn_before_its_resolve_is_answered_sends_nothing_for_its_channel`, `a_post_key_for_a_row_the_frame_has_not_followed_posts_nothing`, `typing_at_the_first_frame_begins_the_draft_in_the_channel_drawn`, `the_title_names_the_channel_a_draft_is_typed_into_before_the_answer`, `a_draft_typed_at_the_first_frame_posts_to_the_rows_target_once_the_same_channel_answers`, `a_key_for_another_row_never_begins_a_draft_in_the_drawn_channel` and `the_parked_store_holds_eight_channels_of_their_newest_hundred`. Unproven and left: no person has used either landing, and the first frame is unmeasured on the hub; the hub drops a channel's model when nobody watches it, so each return still costs one Slack read; the window reads the same page a second time after the hub's snapshot (`chat.history` repeats the hub's read); messages edited, deleted or written while the row was away show as last seen until the snapshot is read again, and a row paired again while away shows its old channel for the debounce and one resolve round trip; a reader who had paged back loses the older pages on return, and a parked channel trimmed to 100 loses the records the Slack plan has hidden since they were held (it moves with time), which the next page then says (`your Slack plan hides older messages`); the window has no deadline for the unconfirmed state (it ends at the answer or a link change); a seeded channel name outlives a rename in Slack under a hub that never names the channel; a row the listing closed inside its first frame is still named by the title, the foot and a typed key until the next frame (`Wanted.unconfirmed` is a second copy of the remembered answer); after an answer for another channel the foot names a kept draft's old channel by its id; the daemon tells a connection nothing when a pending snapshot completes, which is why the window asks on a clock.

- **LOCAL-GATE-OMITS-ITS-SCOPE-SUITE** (distribution) [note] closed by the landing that fixes it, 2026-11-20, from two hosted reds the local gate had passed (2026-11-19 and 20; the second after a release candidate was cut from that commit): `scripts/tests/test-rc-gate-scope.sh`, the suite that proves the scoped gate's own lists, ran only in the hosted checks job. The local gate now runs it as one of its parallel shell jobs (`rc-gate.sh`): in every whole run and every widened one, and in a scoped run whose paths yield the piece `gate scope-suite` (`gate_suite_pieces` in `lib-gate-scope.sh`: any `rust/<crate>/**/*.rs` or crate `Cargo.toml`, and any file under `scripts/tests/`); not for a docs or Julia change alone. The job is a shell job under the gate's `env -i` environment, so none of the run's `RCG_` variables or its scope reach the suite's own runs of `rc-gate.sh` (the job's `PATH` still leads with the gate's tool folders, which is where the suite's git comes from); the suite in a fixture meets a stand-in for itself, so it never starts itself again; the job's 1200 s bound is the gate's ordinary one, and its failure is the run's (`ALLDONE FAILED`, the suite's key in the summary). Pinned by rows of the suite's path table and by the fixture cases `sg1` to `sg3`. Limits: the suite run alone from a shell inherits the shell's `RCG_` variables (the gate's `env -i` job does not); the inner runs of `rc-gate.sh` meet a stand-in for the suite rather than the suite; the suite takes about 50 s alone, and a whole run of the gate with it took the same 7 minutes as the run before it; a Rust change of any size now pays that job, in parallel with the others. The hosted step stays.
- **WIN-ESCAPE-CASE-FILE-CRLF** (wire) [note] closed by the landing that fixes it, 2026-11-20, a red in the hosted Windows run on the commit that added the test, no use behind it (`escapes::tests::the_random_cases_are_what_the_grammar_gives_and_cutting_at_the_held_tail_never_splits_a_sequence`: "escape_random.tsv is stale"): the test compares the generated case file byte for byte with what it makes with `\n`, and a Windows checkout wrote the file with CRLF. `.gitattributes` now keeps both terminal-escape case files at LF on every system, as it does the golden files. Not run on Windows before this landing: the hosted Windows job is its proof.
- **GATE-SCOPE-LIST-MISSES-ESCAPES-TEST** (distribution) [note] closed by the landing that fixes it, 2026-11-20, a red in the hosted checks job on the commit that added the test (`test-rc-gate-scope.sh`: the tree-reading list did not hold `rust/protocol/src/escapes_tests.rs`, which reads the REPL child's case files from outside its crate): a scoped run for a change to those case files would not have run the wire's tests that read them. The list has the row now, under the piece `julia/repl` paths yield.

- **WIN-ATTACH-TEST-LINES-VIEWERS-UP-FROM-THEIR-ENDS** (capsule) [note] closed by the landing that fixes it, 2026-11-19, a red once in a hosted Windows run, no use behind it (`windows_only::attach_mid_stream_snapshot_reproduces_reference_screen`: the late viewer's bytes were not the tail of the reference's): the test stopped reading the reference viewer a second after the late viewer went quiet and compared the two streams from their ends, and that time the reference had not yet received the run's last bytes (the pseudoconsole's closing sequences, which the late viewer had), so the ends did not line up. Both viewers are sent the same frames by one call, and a watcher's attach makes no call into the pseudoconsole. The test now publishes an end-marker frame through the same fan-out after the run returns, reads each viewer to it, compares frame for frame and prints each viewer's delay to the marker; a lost, gained or reordered frame still fails it. It compiles for Windows on Linux and passed once on hosted Windows before it landed.

- **CHAT-FILE-POST-TEST-SHORT-DEADLINES** (chat) [note] closed by the landing that fixes it, 2026-11-19, a red once in a hosted run, on a macOS runner, no use behind it (`only_the_share_can_be_unknown` got `Refused("timeout")` for the `noid` case, which hangs nowhere): the test gave every step of a file post a 300 ms deadline so that its `hang` cases would end quickly, which assumed that the ask (a read), the upload (a transfer) and the share (a post) each answer inside 300 ms; a slow computer's round trip over that was a timeout before the share was sent. Forced by delaying the `noid` ask 400 ms in the fake (red 20 of 20 on the old test). The test now builds an `Api` per case with the short deadline on the step that case hangs at alone and 10 s on every other, and each case is held to an elapsed bound, so a slow step is slow and never a timeout, and a deadline wired to the wrong step is red. The same assumption was found in four more places, two by a review that forced a stalled answer: `chat_secrets_stay_inside_daemon_boundaries` (the 400 ms bound on the answered `malformed` and `redirect` requests; now only `hang` has it) and the channel reads' shared wait in `the_reads_of_the_channels_an_app_cannot_read_end_at_their_bound` (400 ms, now 1 s); the names deadline of two name tests (400 ms, now 1 s) and the read deadline of `an_oversize_page_halves_and_is_read_again` (400 ms, now 1 s in its "too slow" runs, an ordinary one in its flood runs). Those last three, and the channel reads' wait, are widened and not freed of the clock: one deadline covers the hung step and the answered ones by design, so the answered steps must still finish inside 1 s (a lookup takes about 44 ms here, 20 times under it; unmeasured on a slow runner).
- **CHAT-MAIL-TEST-INBOX-SWAP-GAP** (chat) [note] closed by the landing that fixes it, 2026-11-19, a red once in a hosted run, on a Windows runner, no use behind it (`an_unreadable_inbox_at_start_files_nothing_until_it_reads` found post 1 filed again): the test made an inbox unreadable by a directory in its place and made it readable by `remove_dir` then `write`, which assumed no reader looks between the two calls; between them the inbox is absent, an absent inbox reads as empty (`lines_from` skips a file that is not found), and the first pass, finding no record of post 1, filed it again, which is the product doing right for what it read. Forced by a 500 ms pause between the two calls (red 20 of 20 on the old test). The start's read is all or nothing (it fails at an inbox it cannot read and keeps nothing across tries), so the test now makes two inboxes unreadable, the one holding post 1's filing and a second, restores the first in the same two calls while the second is still unreadable (a read in the gap still fails whole), and one `remove_dir` of the second makes every inbox readable at once. It keeps every pin of the old test, among them that an inbox that failed and then reads has its content counted (a mutant that drops it is red 20 of 20), and is green with a 500 ms pause in the gap, 20 of 20.
- **EDIT-KEY-RESHAPES-WHOLE-FILE** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, measured by a review, not seen in use by a person: in the preview's edit mode a key that changed the text, the selection's range or the footer's words shaped the whole file again (about 500 ms per key on a 5,000-line, 465 KB file in a release build). The selection is now drawn as bands under the rows from the index, not as colored text, so a selection step shapes nothing (the visible change: a dim gold band of the edit view's own, (61, 59, 37), where the selection was amber text, the edit text reading 7.98 : 1 over it and the band 1.75 : 1 against the ground, and the cursor's bar opaque in the text's color). An edit brings the shaped buffer to the text in place: one record of what the text's changes were (`EditBuffer`'s `Change`, folded from every splice, an undo or a redo told by the two texts' common start and end), turned into lines by the one line-end rule (`buffer::line_change`), replaces only those lines and the footer's when its words differ (`replace_buffer_lines`, a third door into the shaper in `render/text.rs` under the same rule and the same clippy group), and cosmic-text shapes only lines with no cached shaping; the index is built again from the cached layout (0.44 ms). Measured on a 5,000-line file of about 90 characters a line, release build, on a second computer: a typed letter, Backspace, Delete, Enter, a paste of one line, an undo and a redo each under 1 ms from the key to a buffer ready to draw (845 to 870 ms before), a selection step under 0.001 ms (about 870 ms before). What remains: a key shapes the whole line it lands in, so a file of one long line is as slow as before (853 ms a key on a 446 KiB line, measured by the review; EDIT-KEY-RESHAPES-A-LONG-LINE), and a key's cost still grows with the number of lines, about 0.1 µs a line (5.5 ms at 50,000 lines), from the index's rebuild and the shaper's walk of every line. A walk of 1,000 random keys, with reloads, width and scale changes and restored edits among them, holds the in-place buffer equal to a fresh whole-text shaping and the bands to the selected glyphs, and per-thread counts pin the lines each key shapes. The whole-text shaping stays for opening an edit, a reload, a zoom and a row's restored edit. Unproven: no person has used it, and the paint of the bands rests on reading.
- **CHAT-WATCHERS-TOLD-BEFORE-THE-CHAT-IS-OFF** (chat) [note] closed by the landing that fixes it, 2026-11-19, a red once in a hosted run, on a macOS runner, no use behind it (`the_chat_is_off_with_its_code_before_a_watcher_is_told_the_service_ended` failed its last round of 60: `Stopping` where `Failed("wrong_app")` was required): when a service ended on its own, the owner waited 1 s (`2 * STOP_WITHIN`) for the chat to read off with the code before telling its watchers, and the stop it had begun runs on a task of its own whose join of the listener alone may take half of that; a stop that took longer than the rest of that second (by how much in the red run was not measured) told the watchers while the chat still read `Stopping`, and a window's Chat drawer then showed `reading…` where the Connect step belongs, until a later watch was refused (within about 5 s of the stop's end). The wait is now the stop's own bounds and `STOP_WITHIN` more (`MAIL_END + 2 * STOP_WITHIN`), and the test holds the stop at a hold point (`chat.unit.off`) past the old bound instead of running 60 rounds: red every time without the longer wait. In use since 2026-11-17 (the rule's landing); not seen by a person. A stop that outlasts the new wait (about 16 s) still tells its watchers early, with the same recovery and a log line; while a stop hangs, the watchers now hear nothing for that long.

- **CHAT-MENTION-NOT-A-MENTION** (chat) [person] closed by the landing that fixes it, 2026-11-19, seen in use by a person (the maintainer reported that an `@person` mention in Slack did not work) and by a session asked to post "@name" and a message, which posted the literal text: the daemon escaped every post's text and sent no mention. The daemon now links each `@name` of a post, a thread reply or a file comment, from a person or a row, to a member of the post's own workspace by the rule of chat guarantee 35 (a whole name, display or real, beats a first name; a longer whole name wins only when it is a shorter one plus further words; one member with that first name links; anything else is several members, with the members named), from a `users.list` walk under the bot's `users:read` scope (no new scope), kept 10 minutes, at most ten pages, read within the 10 s names deadline; every mention made is told in the reply's optional `mentioned`, each member's names cleaned of quotes, marks drawn like them and invisible characters and put in double quotes, every entry ending with the account's handle and each list with its count, so no name a member chooses can forge the report, and a name with no member or several, or a list unread or too large, stays text and is named in its optional `unlinked` (the window's status line and `sot-fe chat post` print both, and the skill tells the agent to read the mentioned names back); a person's `@here`, `@channel` and `@everyone` notify and a row's never do; nothing typed reaches Slack unescaped (chat guarantee 35, `rust/chat/src/mention_tests.rs`). What the rule cannot prevent, each shown by `mentioned`, is stated in the contract and the manual (a first name typed for someone outside the workspace or for a member whose first name is only in their display name, a name that is another's plus further words, a following word that completes a longer name, a whole name equal to another member's first name, a rename within 10 minutes); a post whose answer was lost reports no mentions (CHAT-MAY-EXIST-NAMES-NO-MENTIONS). Used: the maintainer posted a mention from the Chat drawer of the 0.7.3 candidate on 2026-11-20 and reported that it works, and that a poster has to remember the name's exact spelling (CHAT-MENTION-NO-AUTOCOMPLETE). In the tests the fake Slack stands in for `users.list` and for how Slack renders `<@U...>` and `<!here>` in a post made with a user token or the bot's token.
- **EDITOR-TESTS-OUTRUN-THE-MAC-SHARD** (fe-ui) [session] closed by the landing that fixes it, 2026-11-19, seen by a session reading the hosted run of the editor's landing (the macOS 26 and macOS 27 jobs of one test shard ended at the step's 5 minutes with every test passed; three editor tests each ran past 60 s there): the editor's test harness made a font system for every text of its exhaustive tests (about 1,800 of them) and re-shaped the text at every read of the cursor's place, which the window itself no longer does for a motion. The exhaustive tests now give one harness each text in turn, and the harness re-shapes only when the window would. The editor's tests take 4 s where they took 23 s on one Linux computer (16 s against 55 s on one thread); the time on a hosted Mac is not measured before the landing's own run.

- **EDIT-DISCARD-SHOWS-FILE-BEFORE-LAST-SAVE** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, found by a review's probe of the edit's key path, not seen in use by a person: discarding an edit with the prompt's confirm key closed it through `DiscardAnswered`, whose window arm fetches nothing, where `Closed` (Escape on a clean buffer) fetches the preview again "so it reflects content saved during this edit session". So after a save, more edits and a discard the preview showed the render from before the session until something else fetched it, stale by every save. It was so in v0.7.2 (`git show v0.7.2:rust/frontend/src/ui/preview/editor/state.rs`, the confirm key returns `DiscardAnswered`) and in the code before the refactor 856c1262d, which cleared the edit and fetched nothing. The confirm key now returns `Closed`; a fetch asks for the file as it is, and cannot show text older than the save, which an installed copy of the saved bytes could not promise to render as the preview does. Pinned by `discarding_with_the_confirm_key_closes_the_edit_as_a_clean_escape_does`; the window's `Closed` arm is as it was.

- **EDIT-FIRST-FRAME-READ-ONLY-AFTER-DRAWER** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, found by a review's probe, introduced by the same branch's guard against shaping an edit for a pane with no width: switching to a row with an edit open, from a row with none, under a full-height drawer left the edit with no shaped text, and the first frame after the drawer went decided what the pane shows before it fitted the edit buffer, so it drew the read-only file while the editor had the keys (a key typed there went into the buffer unseen) and asked for no other frame. The frame now lays the pane out and fits the edit buffer before it decides (`State::frame_shows`); pinned by the native fixture `an_edit_footer_says_what_the_edit_is`, red with the old order. Never in a release.

- **EDIT-DISCARD-PROMPT-NOT-DRAWN** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, found by a review's probe of the edit's key path (a test harness over `Editor`, comparing the drawn footer with a fresh shaping), not seen in use by a person: Escape on an edit with unsaved edits raised the discard prompt (`confirm_discard`) but `editor_key`'s arm for `ConfirmDiscard` and `DiscardAnswered` re-shaped nothing, so the footer kept "modified · save · discard" and never showed "DISCARD UNSAVED EDITS?", and the next key answered a question nobody saw (any key but the confirm key keeps editing; the confirm key discards); the dismissal drew nothing either. It was so in v0.7.2 (the arm is empty there, `git show v0.7.2:rust/frontend/src/ui/preview/editor/keys.rs`) and in the code before the refactor 856c1262d of 2026-10-26, where the Escape arm raised the flag and returned after a redraw request with no re-shape; the version that last drew it, if any, was not looked for. Fixed by one rule for every outcome (`apply_editor_outcome` re-shapes if owed, `Editor::needs_reshape` knowing the prompt, the stale banner and the unsaved edits), a landed save's reply doing the same (its footer said modified until the next key), and the frame's own check; pinned by `an_edit_footer_says_what_the_edit_is` (a native fixture, red with the old behavior) and `escape_on_an_edited_buffer_draws_the_discard_prompt_and_any_other_key_takes_it_down`.

- **EDITOR-UP-DOWN-BY-PARAGRAPH** (fe-ui) [person] closed by the landing that fixes it, 2026-11-19, seen by the maintainer, who asked that Up and Down in the editor move by the editor's lines, not to the beginning or end of the paragraph: in the preview's edit mode Up and Down moved by the file's lines, so in a wrapped paragraph they left it. Up, Down, PageUp and PageDown now move to the row above or below on screen at the nearest place to the column, kept across shorter rows (`preview/editor/rows.rs`, read from the rows the window draws, with no shaping of its own), and the view follows the cursor (`ShapedPreview::follow`). The cursor is a bar the render pass draws over the text, no longer a `█` in it, so it cannot move a wrap. A CRLF file is walked by rows and a CRLF is one cursor position, so a selection never cuts a line end; the lines of the body are the lines the shaper makes (a CR-only file, or a final lone CR, included), by one function for the buffer and the row index; the bar is drawn on every row, right-to-left text included; a step onto or off a row of right-to-left text, or in a buffer that is not the body's text, goes by the file's lines for that step only. The bar is whole at the first column and clipped as its text is vertically, the view never flips in a short pane, the rows a key reads are built once per shaping and named by the text's revision, and the frame lays the edit buffer out again for a new pane width (shapes it again for a zoom); a motion with no selection shapes nothing (EDIT-KEY-RESHAPES-WHOLE-FILE keeps the rest). Built and tested with simulated text only (a walk over fifteen kinds of text); no person has used it.

- **SCOPED-GATE-SKIPS-TREE-READING-TESTS** (distribution) [session] closed by the landing that fixes it, 2026-11-19, seen by a session operating the gate (a frontend landing passed its scoped local gate and reached `main` red on a platform test that reads every crate's sources, REPL-COLOR-SCAN-CUTS-AT-A-TEST-ATTRIBUTE): a scoped run selected only the changed crate's tests, and a path a Rust test reads from another folder (a docs page, the comm library, a launcher script) selected only its own shell or Julia piece. A path now also yields `tree <name>` pieces (`gate_tree_pieces` in `lib-gate-scope.sh`), and `rc-gate.sh` runs the tests of `gate_tree_tests` for them (platform's `test_scan`, capsule's guards, and the readers of docs, `comm/`, `agents/`, `julia/repl`, `scripts/` and the logo) whatever crates were selected; a filter that matches no listed test fails. `test-rc-gate-scope.sh` fails on a Rust file that walks the workspace or reads outside `rust/` and is not listed. Limit: the search knows its forms of reader (`scripts/CLAUDE.md` lists them and the manual step for the rest); a reader only a Windows run executes is covered by the hosted Windows run alone.

- **REPL-DRAWER-PROGRAM-COLORS-DROPPED** (fe-ui, sidecars, wire) [note] closed by the landing that fixes it, 2026-11-19, part of what the maintainer asked for (that the REPL's color scheme match Julia's canonical colors), not built by REPL-DRAWER-COLORS-NOT-JULIAS: the labels of `@info`, `@warn`, `@error` and `@debug`, and any color a program prints, showed uncolored in the REPL drawer, because the row's Julia ran with its streams on pipes and no color and the window stripped terminal escapes. Color now follows who asked for the eval, by a flag on the request: the window's own `repl.eval` and `repl.run_file` carry `color: true`, and the REPL child prints that eval's stdout and stderr through `IOContext`s that say `:color => true`, makes its error message and its shown value with color, and never ends a frame inside an escape sequence (a tail an unfinished sequence leaves waits for the rest for at most 4096 bytes or a tenth of a second of the program's quiet). An eval another program sends (`repl.execute`, `sot-fe repl eval|run`, `run --fresh` included) carries no flag and runs as before, the stack its code prints included (every frame and signature, which the choice appears in none of), so what an agent reads is unchanged. A window says in its hello that it reads terminal escapes (`reads_terminal_color`); the daemon sends every other connection, an older window included, the same frames with every whole escape sequence taken out and any other ESC as a space, so it is sent no ESC byte (pinned against v0.7.2's own cell rule). The drawer reads SGR into cell styles through the Terminal drawer's color map (`drawer/repl/sgr.rs`), carrying a color across the frames of a stream and never across streams or evals, honors erase in line (erased cells are spaces in the base style, never a selected background) and cursor to column 1, drops every other sequence (a string a stray opener leaves unfinished is text, every line in it kept), and wraps by styled run; the grammar for escapes is one linear function in `sot_protocol::escapes`, bounded at 4096 bytes a sequence, shared by the window, the daemon and the child's tests, and a line a `\r` sent back costs what is written to it. Limits (the manual's list, `docs/src/guide/repl.md`): a tab is one space and every other control character a space; a cursor move other than to column 1 does nothing, so a program that moves up shows its redraw as new lines; a wide glyph and an accent written as its own mark are one cell each in a row a `\r` has sent back; erased cells are plain; underline, italic and conceal are not drawn; no selection or copy; also: the stack under an error's message is drawn without module or argument colors; user code in a person's eval sees its `stdout` as an `IOContext`, not a `PipeEndpoint`, and `get(stdout, :color, false)` is true; a value whose `show` prints other text when given color reads that way in the drawer; a sequence a program splits across two writes more than a tenth of a second apart reaches an older window as two halves, each cleaned; built and measured on simulated text and a Julia 1.13 child, not seen by a person.

- **REPL-DRAWER-ONE-ROW-PER-WRITE** (fe-ui) [session] closed by the landing that fixes it, 2026-11-19, found by the review of the drawer's colors by probes with the real child, no person: the drawer drew each frame of output on a row of its own, so `print("a"); print("b")`, a `Pkg` status line written in pieces and a progress meter's repaints each showed one row per write where a terminal shows one line. The drawer now continues the line a stdout or stderr write left open with the next write of the same stream (one run, read write by write by a reader the window keeps for it, and cut again only in the line it is in), so those are one row each and a `\r` repaint, erased by `ESC [ K` as a meter sends it, overwrites the row it repaints (a meter that redraws several lines by moving the cursor up, as ProgressMeter's `showvalues` does, draws each redraw as a block of new lines, since the drawer does not move the cursor); any other frame between ends the line, so output of the two streams interleaved shows each write on its own row as before. Limits: a line the program holds open past 64 KiB characters goes on in a row of its own, which nothing marks; a write that ends a line ends its run, so the start of a title or a link written with a line break, whose end comes more than the child's hold (0.1 s, 4096 bytes) later, shows the start as text, where a terminal would swallow the string through its end (the other order, the wait before the line break, is one line with the title taken out; an agent's eval cut where its pipe was shows the same text; an older window shows the same text); built and measured on simulated frames and the child's pieces, not seen by a person.

- **REPL-DRAWER-STACK-ROW-CLIPS** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, found by the review of REPL-DRAWER-CLIPS-LONG-LINES, never reported by a person: a stack frame's `at fn (file:line)` row was cut at the pane's edge. The error's message lines and its stack frames are now logical lines of one cached set of rows, a stack frame's runs keep their styles across the cut and its continuation rows sit four cells in, under the `at`, and a reader inside a stack trace stays on the same frame through a resize.

- **REPL-DRAWER-CLIPS-LONG-LINES** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, found by the review of REPL-DRAWER-COLORS-NOT-JULIAS, never reported by a person: the REPL drawer cut a line longer than the pane at its edge, so the end of a long error, a value, a line of output or the line being typed could not be read in the drawer. A line now continues on the next row at the pane's width in whole cells (`drawer/repl/wrap.rs`), cut by cell and by grapheme cluster as ratatui's buffer draws them, never in the middle of a flag or a family emoji; typed code's continuation rows, in the log and in the live input, sit under the code, the cursor on the last row (on a row of its own when that row is full), the input's height counting its wrapped rows up to the drawer's cap, and the scrollback gives up what the input takes. A scrolled-up reader stays on the same text through a resize, anchored by the source of the view's bottom row. The drawer also rebuilt every row of the whole log on every redraw (12.7 to 14.7 ms for 10,000 lines of 200 characters in a release build); the wrapped rows of each text frame are now cached (a second copy of the text, about three times its size), so a redraw that changes nothing takes 1.7 to 3.5 ms and a new width re-wraps the log once (17 to 26 ms for ASCII text, on loaded machines; 54 to 61 ms when each line holds a non-ASCII character; later cut to 7 to 9 ms and 23 to 27 ms by the run-based wrap, see REPL-DRAWER-PROGRAM-COLORS-DROPPED). Every row the drawer builds goes through the one cut (the origin marker of another session's run, the browser link, the image caption and the notes among them), and, since REPL-DRAWER-STACK-ROW-CLIPS closed, a stack frame's row. Limits: the drawer has no selection or copy (REPL-DRAWER-NO-SELECTION-OR-COPY); a pane no wider than its prompt does not wrap that prompt's line; built and measured on simulated logs, not seen by a person.

- **SOT-FE-CHAT-WORDS** (agents) [note] closed by the landing that fixes it, 2026-11-19, by reading and confirmed on the stub daemon of `agents/tests/test-sot-fe-chat.sh`: `_chat_resolve` in `agents/sot-fe/sot-fe-chat.sh` had no arm for the daemon's `unconfigured` state, so a hub where no Slack workspace was ever connected printed `refused invalid the chat's configuration does not map this session's project`. It now prints `refused unconfigured <words>` by the daemon's reason (`not_connected`, `tokens_refused`, `wrong_app`, `start_failed`, any other); the exit code (2) and every other word are unchanged (test: `case_a_resolve_that_maps_nothing_posts_nothing`). The exit-3 words, which ended "read the thread first" (no verb reads a channel), now end "ask the person to look in the channel", in the script, its suite, `agents/CLAUDE.md` and both `sot-comm` skills.

- **CHAT-POST-HARD-TO-FIND** (agents) [session] closed by the landing that fixes it, 2026-11-19, seen by a session operating the product: asked by the maintainer to put a name and a message in its Slack channel, a session needed about eight tool calls to find `sot-fe chat post --stdin`, which then worked. No skill's description had a trigger for such an ask; the only mention of the verb was a table row in `sot-comm`. A skill of its own, `sot-chat-post` (`agents/claude/sot-chat-post/` for Claude rows, `comm/adapters/codex/skills/sot-chat-post/` for Codex rows, byte-identical and pinned so by `agents/tests/test-skill-copies.sh`; the installer's directory walk installs both, so no installer list names it, though the manual's install page lists the installed skills and the uninstall line by hand, now updated), carries the asks' words in its description ("post this in your Slack channel", "send this to the channel", "slack this to the channel") and in its body the one route (text on standard input), a thread, a file, the exit codes, what each no-channel refusal prints, that the verb posts only to the row's own channel and publicly (another channel or a direct message is refused by the skill, not posted to the project's channel), and what an `@name` does (CHAT-MENTION-NOT-A-MENTION). Built and not seen: no session has yet been asked in plain words with the new layer installed; that Claude Code and Codex show the description to the model unasked is theirs, not proven here.

- **FULL-DRAWER-HIDES-ITS-ANSWERS** (fe-ui) [note] closed by the landing that fixes it, 2026-11-19, found by the review of the cycle's landing by probes, never seen in use: a full-height or maximized drawer shows the status line on its own top border (`ChromeView::border_status`: the words from the end of the title to the next pane, cut with `…` and lost after their 10-second hold), and only an answer that is the preview ends what covers it, full height and maximize alike, decided by what was drawn (`State::reveal_preview`): a chat picture's original asked for and its arrival, and `Primary+u`, which shows the covered preview and posts nothing on that press (and with no preview pane in the layout says so and posts nothing). A first fix that ended full height at any word a key wrote was not landed. Built and pinned by view tests and the native fixture `a_full_height_drawer_shrinks_only_for_an_answer_that_is_the_preview`; no person has used it; in a layout with no preview column nothing is uncovered and the post key posts as before, and the border's words take the room from the title beside the agent pane (a first round-4 version refused the post and lost the words there); a preview column too narrow to lay out still gets "it shows now"; the manual says a layout with the column left of the agent under 2 cells wide has no full height.

- **WIN-CIM-LISTING-30S** (sidecars) [note] closed by the landing that fixes it, 2026-11-19, a red twice in hosted Windows runs, no use: `the_command_line_observer_rejects_a_deliberate_leak` ran past its 30 s bound on the whole-table `Get-CimInstance Win32_Process` listing; the test's Windows tree reader and command-line reader (`repl/tree_argv_tests.rs`) now use the daemon's in-process Toolhelp snapshot and per-process handle read (`comm::registry::ancestors`), with no PowerShell or WMI; on a hosted Windows run of the change the test passed (once in each of the two binaries that hold it); that one run does not prove the timeout gone, which showed in two runs of many.

- **REPL-COLOR-SCAN-CUTS-AT-A-TEST-ATTRIBUTE** (fe-ui) [session] closed by the landing that fixes it, 2026-11-19, seen by a session reading a hosted run (the platform crate's `no_scan_cuts_a_file_at_its_first_cfg_test` failed on Linux, macOS and Windows on a branch that held the REPL drawer's colors): the colors' source scan (`a_color_at_a_use_site_is_not_in_the_drawers_table`) cut each scanned file at its first test attribute, which the platform's scan check refuses because code below a mid-file test module is then never read. It now uses `sot_platform::test_scan::without_test_modules`, as the other source scans do. The colors' landing reached `main` red on this check: its scoped local gate ran the frontend's tests and not the platform's, whose scan check reads every crate's sources.

- **REPL-DRAWER-COLORS-NOT-JULIAS** (fe-ui) [person] closed by the landing that fixes it, 2026-11-19, seen in use by a person on Linux (the maintainer asked that the REPL's color scheme match Julia's canonical colors): the REPL drawer drew `julia> ` light cyan and not bold, a value light green, stderr red, an error's message light red with no `ERROR:` and a stack frame dim gray, none of them Julia's. Every color the drawer chooses is now in one table (`drawer/repl/colors.rs`), from Julia 1.13's own source: `julia>` bold green and `pkg>` bold blue (the live prompt, its cursor and the scrollback), typed code, stdout and a value in the terminal's own color, an error as bold light-red `ERROR: ` and its message in the terminal's color, and a stack frame's function bold (except `top-level scope`) with the rest light black, not dim; the named colors map to RGB as the Terminal drawer's do (ui/CLAUDE.md, `presentation::tests`, `colors::tests`). One departure on purpose, not the maintainer's word: Julia's REPL prints stderr in the terminal's own color, so a log read back later cannot tell it from stdout, and the drawer keeps stderr gray (ANSI 7), a color Julia's REPL uses for nothing; the window's own "connection is back" note is light magenta. Built, and used by the maintainer on the 0.7.3 candidate on 2026-11-20, who reported it working. The manual's still of the drawer (`docs/src/assets/media/repl-figure.png`, shown in `guide/interface.md`, `guide/figures.md` and `start/tour.md`, and cropped for the readme by `docs/src/assets/readme/make-crops.sh`) shows the old palette until its media is redone. Julia's coloring the drawer still lacks: the logging macros' labels (`@info` cyan, `@warn` yellow, `@error` light red, `@debug` blue) and every color a program prints itself, because the REPL child's streams are pipes that Julia does not color and the window drops the escapes it is sent (keeping them takes the child coloring its streams, `julia/repl/src/frames.jl`, and a window-side reader of SGR sequences; not sized); a stack frame's module and argument colors and its underlined location (the child sends only function, file and line, and no cell run underlines), and a frame Julia prints as `top-level scope` from its code info under another name (the child sends the name alone); the syntax coloring of typed code; `help?>` and `shell>`, which the drawer has no modes for; the `JULIA_*_COLOR` variables; and wrapping: the drawer clipped a long line when this landed, so a long error's first line lost 7 more cells to the `ERROR: ` label, which stays inline as Julia prints it (REPL-DRAWER-CLIPS-LONG-LINES wraps it since).

- **DRAWER-HEIGHT-NOT-KEPT-PER-ROW** (fe-ui) [person] closed by the landing that fixes it, 2026-11-20, seen in use by a person on the 0.7.3 candidate (the maintainer reported that the drawer's height state was not saved when moving between sessions): the cycle's full height was one flag in `LayoutHelp` beside the shown drawer, parked with no row, so a row left at full height came back at its own height, and a row open at its own height met the full height of the row left; the flag moved into the row's own record (`RowDrawer`: tenant and height, parked by `snapshot_current_workspace_ui` after it settles the flag against the focus, as the drawer's key does, and entered by `enter_row_drawer`), so a row comes back to its drawer as it left it whichever row it was left for, a first visit is closed and a closed drawer is never full. Every way of changing the shown row reaches that save and restore: `SelectedRow::select` is visible to the session module alone and `retarget_workspace` is its one caller. A full drawer entered with the focus on a pane it covers takes the focus, since the frame would end the full height at once, but only for a switch a person made (the row-step keys, Enter in the Sessions list) and with no pane maximized; this is a building session's choice, not asked of the maintainer. The limit, plainly: a session's full height is forgotten when a request other than the person's own step shows it (`sot-fe goto`, a control cycle, an urgent result, a row that closes) while the keys are on a pane full height covers, and the same when a pane is maximized at a person's step. Nothing of a row's drawer is written to disk, so a restart keeps none of it, as before. Built and pinned by unit tests (the tenants with and without Help on the laptop preset; every preset and focus for the Terminal only; an arrival nobody asked for or over a maximized pane; a step under Help; the park after a stale flag) and the native fixture `a_rows_drawer_height_stays_with_its_row` (each way of changing the row, the person's with a person-driven call and the others without, and an edit open on the arriving row shaped by the first frame), red before the change; the maintainer used it on the second 0.7.3 candidate on 2026-11-21 and reported that it works, and the fixture drives the handlers the keys call (`cycle_workspace`, `session_enter_key`), while the row-step key handler's hand-off of the person flag to `cycle_workspace` rests on reading (a winit key event cannot be built); no pointer press reaches the nav tree, so a click is no way of changing the row. Not changed: maximize, the wide preview and a preset's configured height stay window-wide.

- **DRAWER-KEY-CYCLES** (fe-ui) [feature] closed by the landing that fixes it, 2026-11-19, asked for by the maintainer for 0.7.3 (the go-ahead); the three open questions (full height beside the agent pane rather than the whole window, all four drawers, another drawer's key keeps the height) were put to him as defaults and are unanswered, so they are the building session's: a drawer's key (`Primary+k`, `t`, `j`, `m`) opens it at its own height, a second press makes it the full height of the columns it spans (`layout::compute`'s `drawer_full`; the agent pane and the columns right of it do not move; a preset with the agent column first has no full height and its key goes from open to closed), a third closes it; another drawer's key swaps and keeps the height; Help stays at the drawer's height, and full height is drawn only while the focus is on a pane it leaves drawn. Built and pinned by unit tests; the maintainer used the cycle on the 0.7.3 candidate on 2026-11-20 and found the height lost between sessions (DRAWER-HEIGHT-NOT-KEPT-PER-ROW), and the Monitor chart's and the Terminal's resize at full height rest on reading (`every_tenant_fills_the_rect_a_full_height_drawer_stands_in` pins the rect each tenant is given).

- **WIN-LAUNCHER-SPACE-ARGV** (distribution) [person] closed by the landing that fixes it, 2026-11-19, seen in use by a person (the Windows launcher did not work for a user name with a space; what was seen and on which release was not recorded): Windows PowerShell 5.1's `Start-Process` joins an `-ArgumentList` array with spaces and quotes nothing, so `launch-sot.ps1` started the window with the pipe name of `--socket \\.\pipe\sot-First Last-local` as two arguments (`\\.\pipe\sot-First` and `Last-local`) and the window dialed `\\.\pipe\sot-First`, which nothing serves (this computer's daemon never reached; by reading, a relaunch's or a converge's handed-over lease never taken up, so the daemon ends its sessions at the handover bound), and started the splash with `-File C:\Users\First`, so no splash showed. Both now go through `Join-SotCommandLine` (`sot-hosts.ps1`, the quoting `Invoke-SotWithin` already used); test `scripts/tests/test-launcher-leases.ps1` case 18.

- **CHAT-MAIL-TESTS-SHARE-THE-REGISTRY-TURN** (chat) [note] closed by the landing that fixes it, 2026-11-19, a red in a hosted run (Windows), no use: `chat_mail_tests::a_restart_after_live_filings_files_nothing_again` timed out waiting for its second filing. A chat filing is the guarded one, which takes the registry lock within what is left of the 2 s inbox wait, and the lock's turn is process-wide; the chat's mail test fixtures held `TURN_TESTS` shared (`TestHome::new`), so other tests' holds of the turn, the other chat tests' filings among them, could spend that bound and refuse the filing `file_failed`, and the fake Slack's empty history left the next pass nothing to file the post from. The hosted log does not say why the filing failed; on Linux another test's holds of the turn across the filing make the same red every run, and so do the chat tests' own filings when each hold of the turn is slowed to 0.6 s. The fixtures that file (`server/chat_mail_tests.rs`'s `world_with` and `mail_two`, `chat/mail_tests.rs`'s `World::new`) now hold the turn alone (`TestHome::exclusive`), as comm's guarded-filing tests do (REGISTRY-TEMPFAIL-LOCK-TURN); the product files a refused live post from the next pass, which reads Slack's history.

- **CONNECTION-LINE-WHILE-DOWN** (fe-ui) [note] closed by the landing that fixes it, 2026-11-17, found while building the hold of STATUS-WORDS-REPLACED-BY-RELIST, not seen by a person: rebuild_connection_status wrote `connected · <host>:<row> · rev N` from ActiveLink, which no disconnect clears, at any computer's re-list, a row switch and a hold's end, so with the selected computer's link down the status line could say connected; with every word held, each word's hold end made it show more often. The connection line (connection_line) now reads the selected computer's link from HostTable::host_connected and, while it is down, says what its last disconnect wrote (HostTable::last_disconnect), as a state line; only that computer's own reconnect brings `connected` back (session::workspace_list::tests, native fixture the_connection_line_never_says_connected_for_a_computer_whose_link_is_down).

- **CHAT-POST-FILE-SILENT** (fe-ui) [person] closed by the landing that fixes it, 2026-11-17, seen in use by a person on v0.7.1-rc.13 on Windows: with a picture opened from the chat in the preview and the Chat drawer focused, Primary+u (chat.post_file) showed nothing. The preview showed the picture with no node, so ChatDrawer::send_file sent nothing and said `not posted: the preview shows no file`, words a person looking at a picture cannot place, and the status line lost them to the next re-list (STATUS-WORDS-REPLACED-BY-RELIST, which holds them now). State::send_chat_file now tells send_file the preview shows a picture with no node (Previewed::ChatPicture), and the refusal says that the preview shows a picture opened from the chat, which is not a file of this project, and to show a file in the preview, then press the key, by its binding's label (compose_tests::a_picture_opened_from_the_chat_posts_nothing_and_says_what_to_do, native fixture primary_u_over_a_chat_picture_keeps_its_words_through_a_relist_until_the_hold_ends).

- **STATUS-WORDS-REPLACED-BY-RELIST** (fe-ui) [person] closed by the landing that fixes it, 2026-11-17, seen in use by a person on v0.7.1-rc.13 through its first case: in the Chat drawer, Primary+u over a picture opened from the chat showed nothing, because its refusal was written on the status line unheld and the next re-list put the connection line over it. Only 13 writes held the line (set, then hold); the connection line every workspace list's reply rebuilds replaced the rest within seconds, and the agent pane's pump wrote its client's status over any word, held or not, at every frame its row printed. Every word is now held for NOTIFY_STICKY (10 s), a newer word replaces it at once, and the same words written again keep their first hold; the connection line, a disconnect with nothing to carry, the agent pane's attach lines and its pump's client line, the Terminal drawer's diagnostic, a refused hello's mirror and a transfer's progress lines are state lines, written only while nothing is held (StatusLine::show_state); a clear ends the hold, and so do the shown computer's reconnect and a switch to another computer's row for a carried disconnect line. The row's answer to the last input (not sent, delivery unknown, run ended) shows in the agent pane's overlay while it stands, not on the status line, and what a re-render or a re-fetch the person did not ask for raises (a failed equation, a preview's failure after a watcher's change or a reconnect, when no request of the person's waits for its answer) shows in the preview's box alone, the pane's title saying `REFUSED, view not updated` after the box has faded (chrome::status_line::tests, session::replies::tests, agent_pane::owner_tests, preview::markdown::replies::tests, preview::replies::routing_tests, native fixtures primary_u_over_a_chat_picture_keeps_its_words_through_a_relist_until_the_hold_ends and a_reconnect_ends_a_disconnect_line_that_carried_a_word).

- **CHAT-RELOAD-RESTARTS-EVERY-WORKSPACE** (chat) [note] closed by the landing that fixes it, 2026-11-17, from the multi-workspace plan's re-cut, reading code, never seen: a reload (every `chat.connect` and every `chat.pair` that changes the file) stopped and started every workspace's chat, so each watching window of every workspace resynced, each delivering channel owed a pass, the reply waited for every workspace's start, and one workspace's stuck stop made the reload start nothing. A reload now restarts only the workspaces whose plan (credentials and mappings) changed, whose file is gone or whose chat is not running; each stopped workspace is waited for on a task of its own and started when its stop ends, so no workspace's stop holds another's reload, start or reply, and the reply waits only for its own workspace's stop and start (chat guarantee 21).

- **CHAT-ONE-WORKSPACE** (chat, fe-ui) [person] closed by the landing that fixes it, 2026-11-17, seen in use by a person on 2026-11-17 (the maintainer, with five Slack workspaces, one per group of repositories): the chat connected one Slack workspace, refused a second's tokens (`other_workspace`) and gave no way to map projects to channels in more than one. One hub now serves up to 8 workspaces, each with its own secrets file, listener, owner and mail, a failure in one stopping no other (chat guarantees 21, 30 and 31); from the Chat drawer a person connects another workspace (`Connect a Slack workspace`), the channel list shows one workspace at a time with Left and Right between them and pairs a project in the one shown, a row whose workspace failed is told which and can move to another's channel (`Ctrl+Shift+K` in the Connect step), the connected screen names the workspace, and the Chat drawer's title names it while the hub serves more than one (fe-ui, the picker, the Connect step and the title). No person has used it yet.

An item closes by moving here as one line: id, subsystem, the fixing commit or landing.

- **MAC-WINDOW-LOG-NOWHERE** (distribution) [note] closed by the landing that fixes it, 2026-11-19, found reading code while landing PAGE-REDIRECT-UNREACHABLE-ON-A-MAC and never seen as a missing file, though a refusal only the window's log named cost an hour on 2026-11-18: the app bundle's program (`app_bundle` in `rust/backend/src/install/unix.rs`, `exec "<the sot-launch wrapper>"`) starts the window through the rendered wrapper with no redirect, and the window logs to its standard output, which macOS gives a bundle's program as a pipe (measured; which process holds the read end was not), so an icon launch left no file to read. Built: `sot_window_log` (`scripts/lib/sot-daemon.sh`), called once by the all-in-one wrapper and, for the wrapper that dials the plan, once by the checkout's `scripts/launch-sot.sh` (an update re-renders only the all-in-one wrapper, so the call has to travel with the checkout), sends the standard output and error to `frontend.log` in the folder of the daemon's own log (`sotd log-path`) and keeps the launch before as `frontend.prev.log`, nothing older, when macOS started the wrapper as the Ship of Tools app: `__CFBundleIdentifier` equal to `sot_bundle_id` (the one owner of the identifier the installer writes into the plist), parent process 1, and standard output neither a terminal nor a regular file, so a detached `sot-launch > file &` from inside the app is left alone. A folder it makes is 0700 as the daemon makes it; every other start is left as it is, and it fails open. Measured on hosted macOS 26.7.2 and 27.0.1 on 2026-11-19 (run 44543383860): a bundle's program started by `open -W` has the variable, parent launchd and pipes for both streams; a child has the variable and another parent; a shell start has no variable. Tests: `scripts/tests/installer-wrapper.sh` (`window_log_trigger`, `_the_file`, `_is_bounded`, `_fails_open`, `_leaves_every_other_start_alone`, `_a_file_or_terminal_is_left_alone_even_when_the_trigger_holds`, `_once_per_launch`, `_reaches_the_old_icon_launcher`, `_remote_launcher`, `_the_folder_is_private_as_the_daemons_is`, and `_the_real_trigger`, which runs the real trigger inside `unshare -Urpf` and prints SKIP where the kernel refuses that namespace) and, on the hosted macOS jobs, `unix_tests::an_icon_started_by_macos_leaves_the_windows_output_in_the_log_file` (passed on macOS 26.7 and 27.0, run 44550010161, in its earlier form). Unproven: no person's Mac has run the icon with this change; whether a click on the running app starts a second wrapper (each start rotates the file, so it then names the two most recent starts); a detached start from inside the app whose output is a pipe or /dev/null is still taken for an icon start; a later macOS that stopped setting `__CFBundleIdentifier` would lose the file, which that test would show. Linux and Windows are unchanged: a Linux desktop entry's output stays in the desktop session's log, and Windows keeps its own files.

- **PAGE-REDIRECT-UNREACHABLE-ON-A-MAC** (platform) [person] closed by the landing that fixes it, 2026-11-18, seen in use by a person on a Mac on macOS 27 with release 0.7.1: every address the window opened showed in Safari as the redirect's 127.0.0.1 address with "the server unexpectedly dropped the connection". Measured on hosted macOS 27.0.1 before the change: a program reading the kernel's TCP table (net.inet.tcp.pcblist_n) got the two xinpgen headers and no record of another process's connection; tests::a_connection_from_another_process_of_this_account_is_mine failed (peer_owner's lookup did not name the child's connection, and serve_own closes a connection whose owner is not named); tests/other_account.rs failed; and Safari did not reach a page through the window's redirect within 30 s. On macOS 26.7.2 the same commit passed all three. The macOS lookup now walks this account's processes with libproc and admits the connection when a process of this account holds the peer's own end; the table reader is deleted. After the change on 27.0.1 the 17 peer_owner tests pass (peer_owner::libproc tests among them), tests/other_account.rs passes with a real other account, and Safari reached a page through the redirect in 2.3 s.

- **CHAT-UNREAD-CHANNEL-SAID-NOTHING** (chat, fe-ui) [person] closed by the landing that fixes it, 2026-11-18, seen in use by the maintainer on v0.7.1-rc.13 (2026-11-17): after Slack was connected again with a new Slack app, which was not in the project's private channel, the drawer said `not up to date: no messages read yet` over the old messages, gave the channel's ID for its name, and its picker lacked the channel and said nothing of it (a pasted link of it said it was not in the workspace), so he chose another channel; the hub's snapshot and reads called Slack's refusal `failed`. Now a read Slack refuses because the app is not in the channel is `not_in_channel`, a watch of it says so, the drawer says to type `/invite @` and the app's name in that channel, the picker says the project's channel is missing and why, and, a few seconds after a connect, the connected screen leads with the channels the new app cannot read and the invite that keeps each, which a task of the hub finds after the connect's reply and `chat.workspaces` carries (`unread`), so the reply waits on no Slack read; until then the screen says it is checking and offers no choice of channel, and if the check cannot be made it says so and what to type. Unmeasured, to check at a person's first use: that typing `/invite @` and a Slack app's name with spaces in a channel, without choosing it from Slack's list, invites the app.

- **PDF-ZOOM-PAGE-PAST-TEXT-CAP** (files) [person] closed by the landing that fixes it, 2026-11-17, seen in use by a person on v0.7.1-rc.12 (a window on Windows, the daemon on Linux; the maintainer reported that zooming in on a PDF with a complex figure crashed the PDF viewer in the preview pane): zooming into a PDF page of complex figures replaced the page with `binary file — N bytes / Not previewed as text (mime: text/plain; charset=utf-8)` and retired its page keys, with nothing in the window's log (the daemon's WARN named the cause; the window's process did not crash); the page rasterized for the zoomed pane was a PNG past 2 MiB, and `try_plugin_preview` held every non-text plugin output to `PREVIEW_BYTE_CAP`, a cap for output that grows with its input, and sent it to the bytes-level reader, which served the PDF's own bytes as `text/plain`. A bounded-output plugin's picture (a PDF page, a video's poster) now ships whole up to 64 MiB, and past it is refused as `preview_too_large` and shown on the status line (`fit_plugin_output`, `preview_gate_tests::a_bounded_plugins_picture_past_the_text_cap_ships_whole`).

- **CHAT-DRAWER-WORDS** (fe-ui) [person] closed by the landing that fixes it, 2026-11-17, seen in use by a person on v0.7.1-rc.12: opening the Chat drawer, the maintainer reported no information about how to set up another workspace and found "new tokens for the hub" meaningless, and found the Connect step's "your app" to be jargon. The picker's last entry read `Connect Slack again   new tokens for the hub`, an unjoined channel's mark read `your app joins it when you pick it`, and the drawer's title, status line and Connect screens carried the project's own words (hub, token, daemon, row, app) with no pointer to where a file, log or guide was. Every string of the drawer now reads plainly to a reader who has never seen the code (the rules are in `rust/frontend/src/ui/CLAUDE.md`, "The drawer's words"): the hub is the computer's name, a token is a "Slack code" except Slack's own field names in quotes, "your Slack app" is defined at its first use on a screen, a pointer names its place (the manual page, the file `sotd.log` on the named computer, `settings.toml` under `[chat]`), the picker's head says the chat connects one Slack workspace for now and another cannot be added yet, and `Connect Slack again` says it gives the same Slack app fresh codes. Wire's `ChatCode::text` words changed where the drawer shows them (a row's agent reads them through `sot-fe chat` too). The picker's head names the workspace since CHAT-ONE-WORKSPACE closed.

- **CHAT-APP-TOKEN-OF-ANOTHER-APP** (chat) [note] closed by the landing that fixes it, 2026-11-17, from a plan's review, reading code, never seen: an app-level token of another Slack app than the bot token's passed `chat.connect` (Slack names a socket's app only in its `hello`), and the listener acknowledged every envelope unread, so the hub's own channels went silent while it took about half of the other app's events (unmeasured). The listener now learns its bot's app from `bots.info` and ends for good, `wrong_app`, on a `hello` or an envelope naming another app, that envelope not acknowledged (chat guarantee 17); the daemon turns the chat off with the code and `chat.resolve` answers `unconfigured` with `wrong_app`; the window resolves the shown row again when its watch learns the service stopped, and the Connect step's first screen says the App-Level Token is another app's and opens the person's Slack apps to copy the three tokens from one app (fe-ui, the Connect step). Unmeasured: that Slack names the app in the `hello` and each envelope and `bots.info` names the bot's app (premise PS8 of the multi-workspace plan); each check fails open where Slack names no app or `bots.info` fails, so a wrong premise costs the protection, never a working chat.
- **CHAT-REFUSED-TOKEN-RETRIED-FOREVER** (chat) [note] closed by the landing that fixes it, 2026-11-17, from a plan's review, reading code, never seen: an `apps.connections.open` Slack refused as a bad token was retried forever with the service `Running`, and a bot-token read refused `token_revoked` read only as `failed`, so a revoked app never led to the Connect step. An open answered with Slack's token code ends the service `app_token_refused`, a bot-token call answered with a code of `TOKEN_REFUSALS` ends it `bot_token_refused` (chat guarantees 20 and 22), a bare 401 or 403 with no code is retried as before; the daemon turns the chat off and `chat.resolve` answers `tokens_refused`; the window resolves the shown row again when its watch learns the service stopped and shows the refused tokens' screen. Unmeasured: that Slack's token codes are definitive (premise PS11 of the multi-workspace plan); if one stray coded answer is not, the chat stays off until the next connect or daemon restart, and the Connect step says so to the person.
- **LAYER-ADD-DIR-TRUST-STOP** (agents) [session] closed by the landing that fixes it, 2026-11-17, seen by the lab's coordinating session on Claude Code 2.1.326: every new Claude leg stopped at startup at "Adding a new directory: <comm home>/claude-layer ... Yes, add it / No, exit", because the recipe adds the layer with `--add-dir` (guarantee 7) and the spawn's trust preparation recorded only the row's folder, under the declared prefix. Measured under a throwaway configuration and a scratch layer, driven in a pseudo-terminal and never answered: no record for the layer, the dialog; a record for the layer (by its path, or by a link to it), or for a folder above it, no dialog. The spawn now also records the ready layer, wherever it lies, when a declaration is read (`folder_trust::ensure_layer_trusted`, agents guarantee 6). Open beside it: TRUST-RESTART-MISSES-ADDED-FOLDER. The run of a real row on the installed daemon is the maintainer's done test.

- **CHAT-FIGURES** (chat) [feature] closed by the records commit that follows its last landing, 2026-11-17. The maintainer asked (2026-11-15) whether figures work in the project chat and whether a session can post a figure to its project's channel; neither did, and he approved the work (2026-11-16). The plan, `docs/history/chat-figures-plan.md`, is built in its ten landings: L1 the chat's share of a file (`Api::post_file`, `rust/chat/CLAUDE.md` guarantee 32; a40ca229d), L2 its read-back as its poster's (905bdcaf2), L3 wire's two ops (3b026a1e8), L4 a row's post of a file (the daemon's `chat.post_file` and `sot-fe chat post --file`; 0c1f09728, bdd4dbeee), L5 the fetch of a picture of the channel (`Api::fetch_file`, guarantee 33; d6f461d98, 5e2351570), L6 a picture's bytes across the wire (the daemon's `chat.file`, fe-net's `OutgoingReq::ChatFile`; fdd00817b), L7 the window's pictures owner (4e5538df2, d795b8210, with the follow-ups 9881723ad and fc7ed5346), L8 the band of tiles in the Chat drawer (2cbb4d18e, 36bcf5060), L9 a picture opened full size in the preview, its original decoded only in the window's bounded drawing child (214e3a7dc, f9b044f2b, ff89a0821, 1b6c26e72, fa6638d0b), and L10 a person's post of the previewed file (`Primary+u`, under the person's own user token; 6999914e1, d786127dc, d6c40ecc1). rc.11 carries L1 to L8 and L10; L9 is on main after it. No person has used the figures yet, so the in-use result of each stays unrecorded.

- **CHAT-TEST-VERBATIM-FOLDER** (chat) [note] closed by the landing that fixes it, 2026-11-16, a red in a hosted run (Windows), no use: the setup tests' end-to-end test loaded `chat.toml` through `std::fs::canonicalize`, which on Windows gives the verbatim spelling (`\\?\C:\...`), and compared it with the daemon's spelling, which `canonical` de-verbatims. The test now loads through the daemon's own canonicalizer (`chat::canon_of`); no other chat or setup test used `std::fs::canonicalize` in its place.
- **CHAT-SETUP-NEEDS-FILES** (chat) [person] closed by the landing that fixes it, 2026-11-16, seen by a person reading the guide: setting the chat up took making and editing `chat-secrets.toml` and `chat.toml` on the hub, with a daemon restart for each change, and the guide did not say which steps were once per workspace and which per project. The Chat drawer now connects a workspace (the Connect step, `drawer/chat/connect.rs`: the person makes their own app and pastes its tokens, three since a person's post goes out under their own user token) and picks each project's channel (the picker), the daemon writing both files and applying them with no restart; the guide leads with both.
- **CHAT-GUIDE-ONE-COMPUTER-HUB** (records) [note] closed by the landing that fixes it, 2026-11-16, a note from reading code, never seen: `guide/chat-setup.md` said a person on one computer runs the chat there, but the daemon's hub fact needed `hosts.toml` to parse and name this computer, so a computer with no `hosts.toml` ran no chat. A computer with no `hosts.toml` and a name is now its own hub for the chat (`is_hub`, `a_computer_with_no_hosts_toml_runs_its_own_chat_and_a_broken_one_does_not`); one whose file does not parse is still not.
- **WINDOWS-REPLACE-DELETE-PENDING** (platform) [note] closed by the landing that fixes it, 2026-11-16, a red in a hosted run with no use behind it (Rust run 43888855282, windows-latest, `two_writers_of_one_name_each_publish_a_whole_file`, a reader's `read` answered `Os { code: 5 }`): the Windows publish renamed with `FileRenameInformation` first, which leaves the replaced file delete-pending under the name, and a concurrent open of the name fails as access denied; the rename with POSIX semantics was only the fallback after a denial. `rename_into` now tries POSIX semantics first and falls back to the classic replace only on a status that says the file system or Windows has none (`no_posix_rename`). That change did not close the reader side, and its premise was never tested: that the classic replace leaves the replaced file delete-pending under the name and that this is what the reader met. The probe of 2026-11-16 (BENEATH-TWO-WRITERS-WINDOWS-RECURS) found the refused open with the POSIX rename, status file deleted, at the instant of a rename, and the classic replace under a reader refused none in 20,000 replaces (too few to say); no support for the premise. The reader-side rate did not move measurably (2 in 170 main runs before, 1 in 600 after, Fisher exact p = 0.12), the same red has an earlier sighting (run 43172516955, 2026-11-12), and no test was red before the change. The reader side is handled by platform's read asking the open again (guarantee 15). What the change does give, and a test pins, is the replace of a name another program holds open with delete sharing (`a_final_name_a_reader_holds_open_is_replaced`).
- **CHAT-MAPPING-NEEDS-ROW-AT-START** (chat) [note] closed by the landing that fixes it, 2026-11-15, a note from reading code, never seen: `Config::load` admitted a mapping against the rows the daemon had at its start, so a mapped folder with no row then, or a removed one after a restart, refused `chat.toml` and turned the chat off for every project. A mapping is now admitted against the disk: a folder that does not exist is ignored and logged by its ordinal, and a row made later in a mapped folder is admitted at its first action.
- **TEST-CAPSULE-OUTLIVES-TEST** (rows) closed by the landing that fixes it, 2026-11-15, seen by a session on the hub computer: a row capsule a `comm_wake` test started (`sot-capsule supervise` in a user scope, its parent the user manager, two stub `claude` legs under it) ran for eight days after its test process ended without `Env`'s `Drop`, the only thing that ended a test's capsules; its legs carried the runner's `CLAUDE_CONFIG_DIR`, a real account's. Each `Env` now starts a reaper (`rust/backend/tests/support/reaper.rs`), this test binary rerun outside the test's process tree, session and group, which at its pipe's end ends the env's units, daemons (each one whose command line names the env's `--project-root`, in `Env`'s one tracking setter or told by a suite that launches its own) and capsules, and removes its folders unless a process still names one; its limits are the item TEST-REAPER-LIMITS; `capsule_workspaces::platform::a_test_killed_outright_leaves_no_capsule_daemon_or_folder` kills a test with SIGKILL and finds none left. The support's daemon command removes `CLAUDE_CONFIG_DIR`, as a production daemon has none (`support_checks::a_daemon_is_handed_no_claude_config_dir_of_the_runner`).
- **CHAT-SETUP-UNDOCUMENTED** (records) closed by the landing that fixes it, 2026-11-15, seen in use by a person: the manual said how to write `chat.toml` and `chat-secrets.toml` but not how to make the Slack app they need, which permissions it takes, where its two tokens and the workspace and channel IDs come from, or that the app must be added to each channel, so no one could set the chat up from the manual alone. A first-time setup page, `guide/chat-setup.md`, now does, with the app's manifest from the Slack calls and events the chat makes, linked from the chat guide and the configuration reference.
- **AGENT-PANE-ROLL-RESHAPES** (fe-ui) closed by the landing that fixes it, 2026-11-15, seen in use by a person on v0.7.1-rc.8 and rc.9: a page in a 75-row agent pane drew about two frames, because every frame the ring moved shaped nearly every run in the window again (523 of 579 runs, about 125 ms a frame at 203 x 73 columns on Linux, against about 4.5 ms for a preview frame). `TextLayer::prepare` reused a buffer only for the run in the same slot, and the blank cells of a terminal row were shaped as text. Runs are now reused by content, wherever the frame puts them (`render::text::ShapedRuns`), and spaces make no run (`project_lines`); on a synthetic styled 203 x 73 pane the one-row roll's projection and shaping measure about 0.7 ms (median, Linux, release build).
- **ROW-PACE-SLOW** (fe-ui) closed by the landing that fixes it, 2026-11-15, seen in use by a person: on v0.7.1-rc.8, a page in a 75-row agent pane was too slow and jerky, while the same person found the page on a v0.7.1-rc.6 window working great. The row pace that SMOOTH-PAGE-JUMPS added (a whole-row roll starting no faster than a row a frame, up to 600 ms) answered a cause that was not the one seen: on rc.6 the agent pane's page did not ease because Claude Code drew fullscreen and took Page Up itself (CLAUDE-FULLSCREEN-SCROLL). The pace is removed, and the agent pane, the Terminal and the REPL drawers page again as rc.6 did, in whole rows on the cubic ease-out over `smooth_scroll_ms`.
- **PARKED-RESYNC-COUNTED** (capsule) [note] closed by the landing that fixes it, 2026-11-15, a red in a hosted run with no use behind it (runs 43731071071 and 43737009742, `a_parked_client_shown_again_asks_for_the_screen_at_once_on_a_quiet_row`, `left: 2` and `left: 3` where 1 was asserted): the parked-client tests of `fe_attach` took a reader that keeps pace with the agent's flood for granted, but the row resyncs any viewer whose reader falls 256 frames behind (guarantee 5), parked or shown, which sends it a screen it did not ask for and less output than the agent wrote; on four CPUs a parked worker was sent extra screens, and on two its sink never filled. The tests now wait for the sinks to fill, the quiet-row test's agent writes until told to stop, the screen a shown worker asked for is the first after it was shown, that a parked worker asks only once it is shown, and once, is pinned on the quiet row, where nothing is resynced, and a full client on screen is held to no screen after it stopped reading.
- **CONTAIN-PING-PID-INTERLEAVE** (containment) [note] closed by the landing that fixes it, 2026-11-15, a red in a hosted run with no use behind it (run 43733562113, `a_dropped_async_child_takes_its_windows_tree`, `descendant readiness exceeded 30 s`): the Windows tree fixtures started `ping` with `Start-Process -NoNewWindow`, so it wrote its replies to the stdout `ready` parses, and the leader's PID line could land between two of them; no line was a number on its own. `ping_tree()` now starts a `cmd` that runs `ping` with its output sent to `NUL`, so the leader is the only writer there.
- **RESTART-KEEPS-MOUSE-MODE** (capsule) [note] closed by the landing that fixes it, 2026-11-15, a note from reading code made red by its test: a new run's parser kept the old program's mouse reporting mode and encoding, bracketed paste, application cursor keys and keypad and a hidden cursor from the screen file, so a window that reads the attach checkpoint would have sent the wheel as mouse reports into a program that never asked for them (a row that ran Claude Code fullscreen, restarted under its classic renderer); `restore` now resets every input mode with the alternate screen, pinned by `a_new_run_starts_with_the_input_modes_reset`.
- **CLAUDE-FULLSCREEN-SCROLL** (agents) closed by the landing that fixes it, 2026-11-15, seen in use by a person, 2026-11-14: on v0.7.1-rc.7 a page in the agent pane did not ease because Claude Code drew fullscreen and took Page Up itself; rows now start it classic. `capsule_supervisor_env` gives a Claude leg alone `CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN=1`, which wins over the person's `tui` setting, and every row launch builds its leg there (`env::tests::capsule_supervisor_env_starts_claude_in_its_classic_renderer`, red before the change).
- **DRAWING-REFUSAL-WORDS** (fe-ui) closed by the landing that fixes it, 2026-11-15, seen in use by a person: `d` over a picture that is not an image file said `drawing: only image files can be drawn on; ...`, which did not say what drawing does or why this picture refuses it, in a status line the person had to look for. The refusal now says what drawing marks up and names the picture (an SVG, a PDF page, a video frame, else a picture) and the two ways to draw (`drawing_over_a_picture_that_is_not_an_image_file_names_the_two_ways_to_draw`), has its own words under the refused-hello overlay, and the preview's own messages show in a box over the preview pane in the help peek's look, held and faded as it is (`PreviewNotice`, `State::preview_says`, `preview::notice::tests`).
- **NAV-STATUS-SCROLLS-OFF** (fe-ui) closed by the landing that fixes it, 2026-11-15, seen in use by a person: with the cursor far down a long nav tree the status lines at the top of the pane were out of view, because they were the head of the scrolled body (`ChromeView::nav_body`). The status and the spacer under it are now pinned at the top of the nav list, the tree scrolling in the rows beneath, and the cursor's scroll and the nav spill count in the tree's own rows. The cursor's row is now the one the tree marks selected, which also corrects the workspace picker's scroll: a fixed offset had counted one or two fewer rows than the picker's path, legend and account rows, so in a short list its cursor could be off the pane.
- **CI-LINUX-SOTD-SHARD-BOUND** (distribution) closed by the landing that fixes it, 2026-11-14, a red in a hosted run with no use behind it: the Linux job that runs the shards `sotd-a`, `sotd-b`, `sotd-c` and `inst` in turn took 270 s and 274 s of its 300 s step bound on two runs of main, then 309 s on a third (Rust run 43675797928), and was cut at the bound with every test it had run passing. The Linux fold in `.github/test-shards.txt` now runs `sotd-a+sotd-b` and `sotd-c+inst` as two jobs.
- **CHAT-RESOLVE-BEFORE-NAME** (fe-ui) [note] closed by the landing that fixes it, 2026-11-14, a note from reading code, never seen: a window that resolved its row before the daemon's start lookup of the mapped channels' names had finished got a `mapped` answer with no `name`, and `RowChannel` kept it, so the title showed the id until a connection change, a changed row or the drawer hidden and shown again; a name the lookup found a moment later never reached it. `RowChannel` now asks again 1, 3, 10 and 30 s after each `mapped` answer with no name (four tries at most; a row or link change starts them afresh, an answer with the name ends them), the answer standing meanwhile, so only the title waits, and `ChatDrawer::next_ask` joins the next try with the owner's so the window is woken for it. No new request and no daemon change.
- **CHAT-PRIVATE-CHANNEL-NAME** (chat) closed by the landing that fixes it, 2026-11-14, seen in use by a person: on v0.7.1-rc.6, the Slack channel still showed as a number, not a name: the Chat title showed the channel's id for a mapped channel that is private. The start's lookup of the mapped channels' names (`Api::channel_names`) went through the mention lookup, whose rule names only a channel Slack says is not private (`channel_name`, `/channel/is_private == false`), so a private channel answered `malformed`, the daemon logged `a channel's name was not found`, the cache held no name and `chat.resolve` carried none. The public-only rule is for mentions, where a message can name a private channel the reader may not be in; a mapped channel is one the hub's owner mapped to a row and the drawer already shows. `channel_name` now takes whether the channel is mapped and is the one place the rule lives: `channel_names` names a mapped channel public or private, and a mention of an unmapped private channel keeps its label or id (a mention of a mapped private one shows its cached name). Tests: `api_tests::a_mapped_private_channel_is_named_and_an_unmapped_one_is_not`, `the_configured_channels_names_are_looked_up_once`, and `chat_ops_tests::a_resolve_carries_a_mapped_private_channels_name`, red before the change.
- **SMOOTH-PAGE-JUMPS** (fe-ui) closed by the landing that fixes it, 2026-11-14, seen in use by a person: on v0.7.1-rc.6 (a Windows window), the page up and down in the agent pane still seemed instant, not a smooth scroll. A page in the agent pane (a third of the pane, about 15 rows) eased over the 150 ms of `smooth_scroll_ms` on the cubic ease-out in whole rows, so at 60 frames a second it showed 5, 3, 3, 2, 1, 1 rows, a third of the page in the first frame, and the Terminal and REPL drawers paged the same way. `ScrollMotion`'s whole-row ask now paces the roll: it starts no faster than a row per frame at 60 frames a second and only slows (its length is the setting or 25 ms a row capped at 600 ms, the longer; its curve the cubic from that start speed to rest), so the page shows one row in its first frame and about a row a frame after, never two, resting in about 300 ms, and a held page key or a burst of wheel ticks rests within 600 ms of its last ask; a wheel tick and a one-row step are unchanged, a zero length still moves at once, and the pixel panes keep their curve (`motion_tests::a_page_rolls_about_a_row_a_frame_from_one_row_to_rest`). `RingRoll` also stops a roll that reaches the live tail with more owed toward it, so no frame is drawn after it.
- **CHAT-RESOLVE-NAME-TEST-RACE** (chat) [note] closed by the landing that fixes it, 2026-11-14, seen in a hosted run (Rust run 43633631706, ubuntu): `a_resolve_carries_the_name_the_start_found_and_none_before_it` failed its count of `conversations.info` calls (2 against 1). The test waited only for the first configured channel's name, while the start's lookup goes on to the second (the rig maps two) and its call can land between the count's two readings. The test now waits until both names are cached, asserts the lookup's two calls, and counts after that.
- **SCRATCH-GUARD-TOKEN-LIFETIME** (distribution) closed by the landing that fixes it, 2026-11-14, seen in a hosted run: on main 82d8f1b97 the Windows job's "Test scratch guard" step passed every check until one threw `could not read the account's profile folder from its token`, and the same step passed on the three commits before. The cause is inferred, not reproduced: `Get-SotRealProfile` read `.Token` from a `WindowsIdentity` temporary with no later reference, so a garbage collection between the read and `GetUserProfileDirectoryW` could finalize the identity and close the handle. The identity is now held in a variable and disposed after the lookup, and the thrown line carries the Win32 error. No red was made.
- **START-TEST-ENV-RACE** (rows) closed by the landing that fixes it, 2026-11-14, a red in the local gate, no use behind it: `only_a_started_claude_spawn_opens_a_leg_and_the_next_spawn_closes_it` failed once with its expected refusal replaced by a preflight error naming another test's removed `sot-createref-env-*` state folder. `spawn_and_watch` resolves the state root from the process environment, and the test neither held `paths::ENV_TEST_LOCK` nor pinned the variables, so it could resolve inside the folder a create-refusal test had pinned and was removing. Reproduced 1 in 400 runs of the two tests together, 0 in 1,200 after. The test now holds `paths::EnvPinned` (moved from the create tests; `fresh()` makes its own folder), and so do 27 other tests that reached `Env::process()` with no lock, found by a temporary probe that logged each such call under one test thread. Left: the background resume a start test's `begin` spawns can still read the config folder after its test ends.
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
- **BENEATH-TWO-WRITERS-WINDOWS** (platform) closed by the landing that fixes it, 2026-11-10: on Windows a write beneath a root kept its renamed file's handle, which shares no deletion, open through the folder's sync, so a second writer's rename over the same name was refused (access denied) for as long as that sync ran and failed once it outlasted the retry window, as `two_writers_of_one_name_each_publish_a_whole_file` did on a hosted run; the handle now closes at the rename, before the folder's sync. The red did not go with it: BENEATH-TWO-WRITERS-WINDOWS-RECURS holds the later reds (2 writer-side in 69 main runs before, 2 in 111 after), so this hold was not the cause of the hosted red, or not the whole of it.
- **CHAT-ROW-SWITCH-LAGS** (fe-ui) closed by the landing that fixes it, 2026-11-10: the Chat drawer lagged a row switch because a stopped watch's records were dropped and a first watch answered `pending` was asked again only after 1 s and at the next idle frame; the chat owner now parks the last two stopped watches that hold records and draws them at a switch back while the channel is read again, and asks a pending watch again at 250 ms and at 1 s, the idle loop sleeping to each ask (superseded by CHAT-SWITCH-RELOAD-TAKES-A-SECOND). Left: a first show still waits for the hub's snapshot and the window's own read, and held records older than the snapshot's oldest are as read, so an edit, a delete or a reply count of one made while its channel was parked does not reach the window (rules and tests in `rust/frontend/src/ui/CLAUDE.md`, "Chat owner").
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
