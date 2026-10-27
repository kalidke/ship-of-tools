# rust/topology: topology (contract, drafted)

hosts.toml declares which computers exist, and everything a process needs to reach one is derived from it in one place:
the endpoints, the ssh login, the window's link that carries a computer's control, pages and rows over that one
login, the relay and pin units (the exceptions are the
limits below: two launcher and installer inputs, the shell clients' copies, a literal path in `sync`, and the
no-argument bridge's endpoint). The library half is the
crate `sot-topology`; the daemon's half (`sotd topology`, `topology.set`, `sotd status`, `sotd same-version`, `sotd
stdio-bridge`, the dial) stays in the `sotd` binary under `rust/backend/src/topology/`, because it needs the roster,
rows and the lease record, which are not crates yet (pass 5c moves the op table into the binary and gives each service
a handler with an explicit context). The crate depends on `sot-protocol` (wire), `sot-platform` (which holds the
local connectors and the `Client` trait) and `sot-capsule` (the capsule's `Endpoint`, which the lane client implements); `sot-protocol` depends on platform alone. The crate starts no child
the daemon owns: the daemon half of `sotd` depends on `sot-containment` and starts its ssh through it (guarantee 5), and
the one start the crate makes itself, the window's link (`Link::start`), is the named exception there. Nothing re-exports it.

## Guarantees
Each states what the rest may rely on, the mechanism it rests on and the limit it has. The Tests section holds the test
of each.

1. **hosts.toml has one grammar and one search order.** `locate` is `$SOT_HOSTS`, else `<config dir>/hosts.toml`
   (`sot_platform::host::state_dir::sot_config_dir`); `load` reads it and `parse` is the only reader of its text
   (`lib.rs`; the daemon's store, the monitor's roster, the CLI and the window call these, nothing else reads the file).
   Exactly one `hub`, which names a listed host with `daemon = true`; a section name is a plain host name; an unknown
   top-level key or section, a duplicate key and a malformed line are errors naming the line; an unknown key inside
   `[host.<name>]` is a warning collected on the `Topology`, not an error. A duplicate key is refused within a section,
   and a `[monitor]` label is refused when any earlier `[monitor]` header gave it. A monitor target (an empty one is
   its label) must not begin with `-` (ssh would read it as an option). Limits: the reader is a subset of TOML
   (strings, booleans, integers, comments), not the `toml` crate; a `[monitor]` target is otherwise any string, since
   it may be `user@host` or an alias; `serialize`, which every edit writes through, is a canonical rewrite, so a
   `topology.set` drops the file's comments and any unknown host key a hand edit added.
2. **Writers of hosts.toml exclude one another, and `topology.set` and `sync` never replace a newer version.** The
   hub's daemon applies an edit (`topology.set`) only where its own loaded file names itself the hub (`not_hub`
   otherwise), and holds `TopologyStore::edit_lock` from its read to its confirming re-read: an in-process mutex and a
   lock file beside hosts.toml (`hosts.toml.lock`, the platform's kernel lock, which fails closed after its bounded
   retry with `io`), so two edits, in this process or another, neither lose an update nor share a temporary file (each
   write goes to a temporary name of its own and is renamed). It re-validates `serialize`d text through `parse` before
   it writes and fires `topology.changed` once through the re-read a hand edit takes, also when it refuses the edit.
   On a box that is not the hub, `sotd topology sync` fetches the hub's file, and replaces the local copy under the
   same lock only if the copy still equals the text `sync` read before it fetched (`publish_synced`); a copy that
   changed meanwhile is kept and sync says so. Limits: a hand edit is a writer outside the lock the daemon only notices
   (at its next re-read, by size and modification time, never a watcher); the rename is not fsynced; whoever reaches
   the daemon's socket may send `topology.set` (the server's peer check and hello are the only gate); a file that is
   currently invalid is refused and the last good parse is kept; `sync` fetches the literal path
   `~/.config/sot/hosts.toml` on the hub, not the hub's own `locate` answer.
3. **A name that reaches ssh from the Rust tree is checked, and no shell runs on this end.** `SshRecipe::new` refuses a
   target or a host outside `[a-z0-9][a-z0-9._-]*` (so none begins with `-`); the argv is built, never a string a local
   shell parses, and the remote command is one argv element. `sotd topology sync --hub` takes a plain host name, a
   monitor target cannot begin with `-` (guarantee 1), `sotd stdio-bridge --host` takes a plain host name, and the
   Windows launcher checks `SOT_HOST_NAME` and `SOT_HOST` against the same grammar (`Test-SotPlainHostName`, run by
   the launcher suite `test-launcher-leases.ps1` in release-gate.yml). Limits: the far end's shell parses the one remote
   command string (the PATH prelude and `sotd stdio-bridge [--host <h>]`, with the host already checked); the
   installer's own `ssh <alias> true` takes the alias a person typed and is the installer's, an accepted exception; the
   shell clients' low-level bridge (`comm/lib/comm-lib-client.sh` `sot_ssh_bridge`) takes its arguments unchecked, its
   callers check them.
4. **Every ssh the Rust tree starts turns connection sharing off.** `SSH_OPTS` carries `ControlMaster=no`,
   `ControlPath=none`, `ControlPersist=no` (a command-line option wins over `~/.ssh/config`), so no master forks away
   from the tree its owner ends; `SshRecipe`'s bridges, `sotd topology sync`, the monitor's sampler and the installer's
   probe start from that list, and the hub's relay command line (`relay_command_line`) spells the same three. Limits:
   the shell client's bridge (`comm/lib/comm-lib-client.sh:237`) deliberately asks for `ControlMaster=auto` and
   `ControlPersist=600`, so the shell clients are outside this guarantee; the relay command line omits
   `ConnectTimeout`, and the relay service has no login bound of its own (systemd's `ServerAlive` pair ends a wedged
   network, nothing ends a login that is merely slow).
5. **A window has one login per dialled computer, and nothing else it does starts one.** `Link::start` is the window's
   only start of an ssh login (`SshRecipe`'s argv, `kill_on_drop`) and of a connection to a hub's relay socket: its
   control connection is the link's channel 0, and every page connection and every lane dial is a channel opened on it
   (`LinkHandle::open`), never a login. Mechanism: `Link::start` is in clippy's process-spawns group, and its one caller
   in the window (fe-net's `connect_and_run`) carries the allow; the lane client and the page proxy have no start of
   their own. The daemon's own logins (the hub link, `sotd topology sync`,
   `dial_and_call`) build their command with `SshRecipe::command` and start it through containment; the monitor's
   sampler and the installer's probe build theirs from `SSH_OPTS`. Limits: a relay route also costs the hub one login to
   the far computer per window (the relay service's, systemd's); the daemon's logins are not the window's; a window that
   dials one computer under two dial keys has two links; a login that is merely slow waits `SSH_LOGIN_BOUND` for its
   `link.hello` answer.
6. **A link's end ends every channel, and a channel opens only on a link that is up.** The channel table lives under
   one lock with the link's state (`link/table.rs`): an open on a link that is down fails `LinkDown` at once and sends
   nothing; the link's end (its input's end, a broken rule, `SILENCE_BOUND` of silence, the end of channel 0 on either
   side, or the `Link`'s drop) marks it down and wakes every waiter under that lock, so every reader of a channel gets
   the bytes already queued and then `LinkDown`, every writer and every open in flight `LinkDown`. A host's
   `LinkHandle` is up only between its control session's accepted hello and that session's end (fe-net writes it), so
   a refused hello opens nothing. Limit: bytes handed to ssh but not yet delivered are lost with the link; nothing
   resends them (an `input` in flight is reported unknown by the capsule's client).
7. **A lane dial is bounded and typed.** A local dial goes through `connect_own` (only an endpoint this OS account
   serves) and its fixed `CONNECT_BOUND` retry budget, which an attempt in progress finishes; its handshake is bounded
   separately (2 s) and cancellable. A link dial opens a channel and runs the handoff hello and `lane.connect` on it, the
   open and the handshake within one `LINK_REPLY_BOUND` (10 s), which holds no login. `lane.connect` refusals stay
   typed (`Refused` ends; `Unreachable`, `Undetermined` and `LinkDown` retry in the caller, `LinkDown` after `link_up`;
   a channel the router refuses is `Unreachable`). A daemon's own answer, a refusal included, is returned as given. The
   lane identity is the daemon's own report (`lane.connect`'s `pid` and `created`, bound only by the hello the dial
   runs over the stream); nothing here re-verifies it, so the dial trusts the daemon it reached. Limits: the link
   bound covers the far daemon's start of a row and the link's round trips, so a link slower than that reads
   `Unreachable` and is retried on the caller's pace; the handshake's bound holds through the stream's own cancel.
8. **An attach is one dial, and so one channel and no login.** A row has one socket, so a remote attach makes one
   `lane.connect` on one channel of the host's link; none is parked for a later dial, and a channel the router refuses
   is the dial's error with no second try inside it.
9. **Endpoints are derived by one function each and never guessed.** `local_endpoint` is `$SOT_SOCKET`, else
   `$SOT_BACKEND_LABEL`'s session socket, else the local label's (`sot` on Unix, `local` on Windows, never a literal);
   `plan`, `dial_endpoints`, `relay_endpoint` and `relay_socket_path` give every other endpoint from a parse and the
   asking host, and `relay_host_for_path` accepts only exactly a generated relay path of a plain host name, so a
   window that reaches a relay socket locally still treats its daemon as remote. A path a unit or an ssh login names
   is derived on the box that owns it. Limits: `relay_socket_path` trusts its caller's host name (`sotd stdio-bridge
   --host` refuses a name outside the grammar before it derives a path); `sotd stdio-bridge` with no argument takes the
   local label's socket and ignores `$SOT_SOCKET` and `$SOT_BACKEND_LABEL` (queue); the scripts spell the labels at
   several sites (`docs/ownership.md`), which the Rust side cannot enforce.
10. **A unit sotd writes starts only where hosts.toml gives it.** `sotd.service` carries a `ConditionHost` pin of the
    hosts that run sotd (`daemon` or `frontend`, `sotd_hosts`), the reverse tunnels' template and each relay socket one
    of the hub; with no hosts.toml `sotd.service` is pinned to the host whose install wrote it (`install_pin`). A pin
    matches the kernel hostname exactly or followed by a dot and a domain. `apply` writes the pins first and stops
    before any enable if one cannot be written. Limits: a box whose hosts.toml name differs from its kernel hostname
    is not matched (`SOT_SELF_HOST` is not seen by systemd); the pins' effect under real systemd condition evaluation
    is tested in `rust/backend/tests/unit_pin.rs`; the generated relay socket units still carry an
    `[Install]` section, so `systemctl --user enable` and `disable` change a link in the home that every host shares,
    which the pin does not stop (the boot-start rule of `sotd.service` has not been applied to them; queue item 17).
11. **Every `systemctl` call topology makes is given a wait budget, and a stuck manager is a named failure.** Each call
    runs through `sot_containment::process().output` with `SYSTEMCTL_BOUND` (30 s) and fails `did not exit within`
    that bound; `apply` collects each unit's failure and runs the rest, `refresh` collects all but a failed reload and
    redoes every step at its next run, and the daemon's refresh at start runs on a thread nobody waits for. Limits:
    the bound is containment's budget with its limits (the child is created before its wait starts, and the timeout
    cleanup reaps without a wall-clock ceiling: `rust/containment/CLAUDE.md` guarantee 4), not a strict 30 s return; it
    is per call, and a complete CLI refresh makes `2 + 3N` calls for N enabled relay hosts (inventory, reload, and
    reset, show and restart per host; the start adds the MainPID probe), so a manager that answers the first and
    stalls the rest can cost minutes (a manager that answers none fails at the first call); `apply` is a dry run unless
    `--yes`. The reported relay-socket hang is not reproduced and its cause is not established.
12. **`sotd stdio-bridge` adds nothing to the stream unless its first frame is `link.hello`.** It connects through
    `connect_own` to the local label's socket (no argument), to the hub's relay socket for a plain host name (`--host`)
    or to a local endpoint a caller on this box names (`--endpoint`), and puts nothing of its own on stdout. The forms
    whose endpoint is local (no argument and `--endpoint`) then read the caller's first line (at most
    `MAX_ENVELOPE_BYTES` and one byte more without a newline, or what came before the end of input, within 10 s, else
    exit 1): a `link.hello` makes the bridge the far end of a window's link (guarantees 14 and 15), the connection
    already made its channel 0; any other line is written to the endpoint unchanged, byte for byte, and the copy starts.
    `--host` never routes (its endpoint is a relay socket, where a connection is a login) and copies from the first
    byte. The copy carries bytes both ways unchanged until either side reaches EOF, and the bridge exits 0 on a clean
    end and 1 on a failure. Limits: it copies until EOF, not for a bound (its caller, ssh or a systemd unit, owns the
    lifetime); a failure usually prints one stderr line, but a host name that contains a newline prints several, and
    the two copy directions can each print a diagnosis; the no-argument form ignores `$SOT_SOCKET` and
    `$SOT_BACKEND_LABEL`; a caller of a local form that says nothing for 10 s is ended (the daemon's own bound on a
    first frame is the same); its integration suite is Unix-only.
13. **Flow control is per channel, so a stalled channel never stops the control channel or another channel.** The
    link (`link/`) keeps each channel's queues and credit under one lock with the link's state (`table.rs`). Each
    direction of each channel has a credit of `WINDOW` (256 KiB) granted at open; a sender writes a channel's data only
    against its credit, in blobs of at most `CHUNK`; the receiver queues a channel's bytes in a queue its credit bounds
    and returns credit, a chunk at a time, as its consumer reads. The link's one reader therefore never waits on a
    consumer, and its one writer (`io.rs`) serves the link ops first and then the channels with data and credit in
    turn, one chunk each. A consumer that stops reading holds at most `WINDOW` bytes of its channel at each end; its
    producer then waits. A peer that sends beyond its credit, a blob over `CHUNK` (refused on its declared length, before
    a byte of it is read), credit beyond the window, an open out of order (the window numbers its opens one after
    another, so every number up to the highest opened was opened) or a frame for a channel never opened ends the link
    (logged once); a frame for a channel already closed is dropped; the open past `MAX_CHANNELS` (channel 0 counted) is refused
    `too_many_channels`. Limits: the bound on another channel's wait is not one chunk but the bytes already in flight
    inside ssh and TCP, at most the sum of the open channels' credit (n x 256 KiB), so on a 1 MB/s link with two bulk
    channels a keystroke can wait about half a second; `MAX_CHANNELS` bounds the queues at 64 MiB per link in the worst
    case; a channel's throughput is at most `WINDOW` per round trip.
14. **A closed window ends its link and every channel within one bound.** Dropping a `Link` ends it, kills its ssh
    child (`kill_on_drop`) or closes its relay connection; the far router reads the end of its input, ends the link,
    every channel and every local connection it made, and exits 0, and the daemon sees each channel's connection end
    as it would any client's. A window killed outright leaves its ssh login with its input closed, which ends the same
    way. Either end also ends a link that brought it no byte for `SILENCE_BOUND` (90 s); channel 0's 30 s ping keeps a
    live link inside it. Whatever ends a link ends every copy over it, whatever the copy waits on: the router's
    channels are tasks its reader owns, dropped with it, and a page's copies select on the link's end, so a local
    connection or a browser that stopped reading is closed too. The router exits when its link ends even while its
    input stays open and silent (it lets its runtime go without waiting for stdin's read, which nothing can cancel). Limits: on a live network the end takes about one round trip; on a dead network it takes up
    to 90 s at the far end (the router's silence rule) and up to ssh's `ServerAlive` 45 s at the window's; sshd's own
    session cleanup is the far system's.
15. **Users stay separate by their own ssh identities; the router adds no path between accounts.** The far sshd
    authenticates the login with the user's own key; the router runs as that account; each channel's connect is
    `connect_own`, which connects only to an endpoint this account serves (platform's guarantee); the daemon admits
    each channel's connection by its OS peer and its hello, as any connection (the server's guarantees 3 and 4). What is
    checked per channel: the account at the daemon's accept, and the hello. What is not: which window or which key
    opened the channel (the hello's host and account are declarations, as today), and any process of the same account
    on that computer may still connect to its daemon directly, as today. The router reads no channel byte.

## Owns
hosts.toml's grammar, search rule, edits and status table; this box's own endpoint, the label, `slug` and the plain
host-name grammar; the ssh recipe, `SSH_OPTS`, `SSH_LOGIN_BOUND`; the link runtime (`link/`: the channel
table, the reader and writer, `Channel`, `Link`, `LinkHandle`, the router's `serve`, `connect_local` and `adopt`,
`LINK_REPLY_BOUND`, `SILENCE_BOUND`, and the window's link login); the lane dial
`DaemonLaneEndpoint`; the unit text sotd generates (the hub's relay socket, service and command
drop-in per relayed host, the pins), the tunnel and relay unit names, and `apply_plan`'s convergence; in `sotd`:
`TopologyStore`, `topology.set` and `topology.changed`, `sotd topology`'s verbs, `sotd status`, `sotd same-version`,
`sotd stdio-bridge`, and the one-shot `dial_and_call`.

## Calls in
`sot_protocol::{op, Frame, Kind, HelloReq, HANDOFF_ROLE, LaneConnectReq, LaneConnectRes, codec, child_lines}` (wire);
`sot_platform::host::{state_dir, env, inherit_none}` and `sot_platform::identity::{challenge, peer, exchange}`;
`sot_platform::transport::{Client, TransportError, connect_own, socket_unix, pipe_win}`, `sot_capsule::endpoint::Endpoint`;
`sot_protocol::ops::link` (the link's payloads and constants); serde, tokio (oneshot, process, time, io), interprocess
(the link's local streams), tracing (the link's end). In `sotd`: `sot_containment::{process, Signal, Contained, ContainedStd, guard_pid}` and the
server's roster and rows for `topology.set`.

## Calls out
The window (`rust/frontend`): `Link`, `LinkHandle`, `LinkRoute`, `Channel`, `connect_local`, `SshRecipe`,
`DaemonLaneEndpoint`, `relay_host_for_path`, `is_plain_host_name`, `local_endpoint`, `session_socket_path`, `slug`,
`SSH_LOGIN_BOUND`; the daemon: `load`, `parse`, `plan`, the unit text, `SshRecipe::command`, and in `sotd
stdio-bridge` the router (`link::router::serve`, `adopt`, `connect_local`); the monitor reads `Topology::monitor_targets` and `SSH_OPTS`; the installers
read `SSH_OPTS` and run `sotd topology pin|plan|sync` and `sotd same-version`.

## Does not
Hold a connection beyond one dial except the window's link (fe-net owns each `Link` and the daemon's hub link its
own; the lane client keeps its streams), decide when anything ends (lifecycle), start a daemon child
outside containment except through the named exceptions, run a shell on this end, decide who may send `topology.set`
(server's admission), or interpret a daemon's replies beyond the lane handshake.

## Platforms
Same on all three except: `endpoint.rs` (a Windows named pipe `\\.\pipe\sot-<user>-<slug>` and the label `local`; a
Unix socket under the runtime directory and the label `sot`), `connect_own` (Unix: the listener's account; Windows: the
pipe server's account at identification level), the unit text, pins and `sotd topology apply`, `refresh` and the
refresh at start (Linux with a user manager; elsewhere they report no manager or do nothing), and `keep_no_descriptor`
(Unix only; Windows children inherit what std leaves inheritable, which is nothing).

## Tests
One per guarantee. Library paths are under `rust/topology/src/`; daemon-half paths under `rust/backend/src/topology/`.

| # | guarantee | test |
|---|---|---|
| 1 | one grammar and search order | `tests.rs`: `v2_parses`, `two_hubs_fail`, `hub_must_be_listed`, `v2_hub_must_be_a_daemon_host`, `unknown_top_level_key_still_errors`, `unknown_host_key_is_a_collected_warning_not_an_error`, `duplicate_keys_and_labels_fail`, `locate_honours_sot_hosts`, `a_monitor_target_that_reads_as_an_ssh_option_is_refused`, `a_monitor_label_repeated_under_a_second_header_is_refused` |
| 2 | one writer, serialized | `set_tests.rs`: `add_flag_remove_each_land_atomically_and_broadcast`, `non_hub_daemon_refuses`, `hand_edit_on_disk_is_picked_up_by_the_next_op_and_broadcasts_once`, `concurrent_edits_each_land`, `a_refused_edit_still_announces_the_hand_edit_it_noticed`; `cli_tests.rs`: `sync_replaces_only_the_version_it_read`, `sync_refuses_a_fetched_file_that_does_not_list_self`, `resolve_hub_prefers_the_local_copy_over_a_disagreeing_flag` |
| 3 | names checked before ssh | `ssh_bridge_tests.rs`: `recipe_rejects_a_leading_dash_in_either_half`, `recipe_rejects_grammar_outside_a_plain_host_name`, `argv_has_no_shell_and_the_stated_option_set`; `cli_tests.rs`: `sync_refuses_a_hub_that_is_not_a_plain_host_name`; the launcher: `scripts/tests/test-launcher-leases.ps1` (Windows PowerShell 5.1, release-gate.yml, not run by the landing job) |
| 4 | sharing off | `ssh_bridge_tests.rs`: `argv_has_no_shell_and_the_stated_option_set`; `relay_units_tests.rs`: `relay_ssh_turns_multiplexing_off_over_an_ssh_config_that_turns_it_on` |
| 5 | one login per computer | `link/tests.rs`: `one_link_carries_control_rows_and_pages_as_channels`; the window's `net/transport/tests.rs`: `a_remote_host_costs_one_login_for_control_pages_and_rows`; `link/tests.rs`: `a_login_that_ends_before_its_answer_names_its_stderr_line`; `dial_tests.rs`: `a_dropped_dial_takes_its_ssh_tree` (the daemon's start is contained); the exception's reason is the clippy allow's; the release gate's pane-timing route counts one relay hop per window run (`ui/app/native_pane_tests.rs`) |
| 6 | a link's end ends every channel | `link/tests.rs`: `an_open_on_a_down_link_fails_at_once_and_sends_nothing`, `a_link_end_fails_every_channel_with_link_down`, `the_end_of_channel_zero_ends_the_link`, `an_abandoned_open_closes_the_channel_its_answer_opens`; `lane_client_tests.rs`: `a_link_dial_in_flight_ends_when_its_link_ends`; the window's `the_link_is_up_after_an_accepted_hello_and_down_when_the_session_ends`, `a_refused_hello_leaves_the_link_down` |
| 7 | bounded, typed dial | `lane_client_tests.rs`: `refusal_codes_map_to_typed_errors`, `a_silent_daemon_is_unreachable_within_two_seconds`, `a_link_dial_on_a_down_link_is_link_down_at_once`, `a_link_dial_opens_a_channel_and_reaches_the_daemon`, `a_silent_daemon_over_a_link_is_unreachable_at_its_bound`, `a_socket_another_account_listens_on_is_refused_by_the_lane_dial`, `a_full_foreign_backlog_ends_the_lane_dial_within_its_bound` |
| 8 | one dial, one channel | `lane_client_ownership_tests.rs`: `a_remote_attach_opens_one_channel_and_starts_no_login`, `a_refused_channel_is_the_dials_error_with_no_second_try` |
| 9 | endpoints derived once | `tests.rs`: `plan_lines_are_stable`, `relay_socket_path_is_a_sibling_of_the_relay_socket`, `only_the_complete_generated_path_is_a_hub_relay`, `a_planned_relay_dial_is_recognized_and_the_hubs_own_is_not`; `endpoint_tests.rs`: `session_socket_path_honours_xdg_runtime_dir` (the launcher's label has no behavior test here); `rust/backend/tests/stdio_bridge.rs`: `stdio_bridge_refuses_a_host_outside_the_grammar` |
| 10 | pins | `relay_units_tests.rs`: `a_pin_names_each_host_and_its_domain_forms_only`, `unit_names_and_pin_paths`, `relay_units_carry_the_host_and_the_declared_path`; backend `relay_units_tests.rs`: `refresh_pins_the_daemon_and_the_tunnels`; `rust/backend/tests/unit_pin.rs` (real systemd condition evaluation, pin-write refusal); the `[Install]` limit has no test |
| 11 | bounded `systemctl` | backend `relay_units_tests.rs`: `a_hung_systemctl_ends_at_its_bound_and_says_so`, `refresh_writes_and_reloads_in_order`, `refresh_after_a_failed_reload_reloads_and_restarts_again`, `the_relay_probe_takes_its_tree` |
| 12 | stdio-bridge | `rust/backend/tests/stdio_bridge.rs` (every test there; the first-line rule: `a_link_hello_makes_the_bridge_a_router`, `a_plain_first_frame_is_forwarded_byte_for_byte`, `dash_dash_host_carries_a_link_hello_unchanged`, `a_first_line_past_the_bound_unterminated_or_crlf_is_forwarded_unchanged`, `no_first_line_within_its_bound_ends_the_bridge`) |
| 13 | per-channel flow control | `link/tests.rs`: `a_stalled_channel_never_delays_another_channel`, `data_beyond_its_credit_ends_the_link`, `a_blob_over_the_chunk_ends_the_link`, `a_frame_for_a_closed_channel_is_dropped_and_one_never_opened_ends_the_link`, `a_skipped_channel_number_ends_the_link_at_the_router`, `credit_returned_beyond_the_window_ends_the_link`, `the_open_past_the_cap_is_refused` |
| 14 | a closed window ends its link | `rust/backend/tests/stdio_bridge.rs`: `a_dropped_link_ends_its_router_and_every_daemon_connection`, `a_silent_window_ends_its_router_with_its_input_still_open` (the bound injected by `SOT_TEST_LINK_SILENCE_MS`); `link/tests.rs`: `a_silent_link_ends_at_its_bound` (both ends, the bound injected), `the_links_end_closes_a_local_connection_that_stopped_reading`; the window's `pages_tests.rs`: `a_link_end_closes_a_page_whose_browser_stopped_reading` |
| 15 | no path between accounts | `rust/backend/tests/stdio_bridge.rs`: `each_channel_is_admitted_as_its_own_connection`, `a_channel_to_another_accounts_daemon_is_refused` (Linux, root: skipped where `sudo -n` fails, run by the hosted Linux job) |

Windows- and macOS-specific arms run on the hosted runner of their OS (`endpoint_windows_tests.rs`, the Windows arms of
`dial_tests.rs`).

## Start here
`lib.rs`'s module doc for the grammar; `ssh_bridge.rs` `SshRecipe::new` for any change to an ssh login;
`endpoint.rs` `local_endpoint` for how a process finds its own daemon; `link/mod.rs` for the window's link and its
router; `rust/backend/src/topology/relay_units.rs` for
what `apply` and `refresh` do to systemd.

## Rules
- Read hosts.toml only through `locate`, `load` and `parse`; build an ssh login only through `SshRecipe`.
- The local endpoint is never the literal `sot`: on Windows the daemon's label is `local`.
- `relay_units.rs` (the unit text) lives in the crate, not the binary, because the backend's `relay_refresh` test calls
  `relay_command_line()` and `sot-backend` has no library target.
- A test build can give an `SshRecipe` a fixture command (`with_test_command`, under `cfg(any(test, feature =
  "test-handshake-bound"))`): every start the recipe drives then runs that program in place of `ssh` (the link's start
  and the daemon's `SshRecipe::command` alike). An ordinary build has no fixture command; `sot-backend`'s dev-dependencies and the window's
  `test-pane-timing` feature turn `test-handshake-bound` on.
