# rust/chat: chat (contract)

The project chat's policy. This stage holds normalization (Slack's raw message JSON into the wire's one record,
`sot_protocol::chat::ChatRecord`), `http`, the one way out of the crate to the network, `api`, the Slack Web API calls
the first stage needs (`auth.test`, history and replies, `chat.postMessage`, `users.info`), and `socket`, the Socket Mode
listener, and `config`, the two configuration files, and `service`, the one running listener, and `watch`, the model of the watched channels. The crate (`sot-chat`) depends on `sot-protocol`, `sot-platform` (the config folder), `toml`, `serde_json` and the web stack recorded below. `sot-backend` depends on it, and `sotd`'s `rust/backend/src/chat.rs` is the only code that names it (the caller paragraph is guarantee 21's). It owns no runtime state beyond `Http`'s client and admission count, `Api`'s
bot identity, read-pacing instant, name cache and closed flag, a `Socket`'s one task, queue and gap, a `Service`'s workspace lock, phase and the claim that makes `Service::watches` give one `Watches` (three states: free, held until its `Watches` is dropped, and ended by a stop for good), and a `Watches`' channels (bounded, guarantee 23), and has no delivery. Its file calls are `config`'s two reads (guarantee 21) and the one lock file `service` takes through platform (guarantee 22); it reads no token from the environment.

## Guarantees
1. **Every message shape gives a result, never a panic or a silent drop.** `normalize(ctx, channel, raw)` returns
   `New` (a message), `Edited` (a changed message under its original identity, so a caller can tell it from new work),
   `Deleted` (the identity to remove), `Unsupported` or `Unrepresentable` (not an object, a bad workspace, channel or
   timestamp, a wrong-typed `subtype` or edit body): the caller treats `Unrepresentable` as a gap. `Unsupported` is neither a gap nor a message: the
   subtype is not on the list of displayed ones (none, `thread_broadcast`, `file_share`, `bot_message`, `me_message`;
   joins, tombstones and any subtype Slack adds later), and the caller skips it and logs the raw subtype. A message with a valid `ts` whose other fields cannot be read becomes
   a record with `omitted` set and no content, not a dropped message; it keeps its author id, its origin (an own post or any message with a `bot_id` key, even an unreadable one, is `Bot`, never
   `Member`) and its thread, each read on its own; an unreadable `thread_ts` falls back to `ts`. Limit: the author's display name is the event's own
   profile name or empty; `Api::fill_names` (guarantee 16) resolves it.
2. **Own posts are decided only by the supplied bot identity, and a forged marker never makes one.** Own-app is
   `Context.own_bot_id` or `own_user_id` matching the message; an identity not supplied matches nothing. Only an own
   post has its origin recovered, in order from `metadata`, then the `username` suffix ` (<kind> via Ship of Tools)`,
   giving `Drawer` (person) or `Row`. An own post with neither is `Bot` (display only), its text untouched, even when
   the text carries a marker. A member who types a marker, a forged `username` or `metadata` from anyone else, and
   another bot stay `Member` and `Bot`.
3. **A record is bounded, and a cut is marked.** 32 KiB of encoded JSON, 8 KiB of text, eight file summaries (the rest
   counted in `files_omitted`, as is any file entry with no valid id), 64 ASCII-byte ids, 160 / 256 / 128 bytes for an
   author name / filename / MIME. A cut falls on a character boundary and sets `truncated` (which covers text, names,
   filename and MIME); the record's size is held by halving the text until it fits, and once the text is gone the other bounds (ids, names, eight files, three timestamps) keep it near 8 KiB, far under the cap, so nothing else is cut. A record's text is Slack's escaped markup (`&amp;`, `&lt;`, `&gt;` stay as Slack sent them), and the byte cut can split a token. Names, filenames and MIME lose control characters and are trimmed of surrounding whitespace (`clean`), and nothing else is stripped: direction and other format characters stay, for the later display and mail rules. A
   file stub (`file_access: check_file_info`) keeps its id only. A private URL is never copied; a thumbnail is a flag.
   Limit: `truncated` does not say which field was cut.
4. **An attachment-only message is a record with empty text and its files.** Nothing needs a caption.

5. **A request goes only to an HTTPS Slack destination, and the bot authorization rides only on those hops.** The first
   URL and every redirect are checked before any request: scheme `https`, no userinfo, port 443, a domain host that is
   `slack.com` or a dot-boundary subdomain with every label non-empty. `evilslack.com`, `slack.com.evil.example`, an IP
   host, `http`, userinfo (including `host:port@evil`), another port and a scheme-relative redirect to a foreign host are
   `Destination`; an unparseable `Location` is `Redirect`. The authorization header is built anew for each hop and is
   marked sensitive. Only a GET follows redirects, at most three; a POST that is redirected, or a loop, is `Redirect`.
   Automatic redirects and ambient proxies are off. Limit: the one predicate is destination and authorization alike;
   a non-Slack unauthenticated destination (a CDN, an upload host) is a later, reviewed addition.
6. **The server is verified, and the product has no way to weaken that.** `Http::new` trusts the bundled web roots
   (`webpki-roots`) through one rustls configuration (ring provider) and checks the certificate chain and the name; either
   failure is `Tls`, before any HTTP byte. There is no endpoint override, insecure mode, test-root setting or plaintext
   path in a shipped build. `Http::for_test`, under `cfg(test)` or the `test-support` feature alone, takes the fixture's
   CA as the only root and a resolver that maps the fixture's logical hosts to loopback and refuses every other name
   (`Connect`, no connection). `Service::start_at` (the fixture's base URL, the product's timing) is gated the same way and held out of shipped builds by the guards below; a base URL given with `Http::new` still reaches only Slack hosts on port 443 (`http_tests.rs`: `the_product_client_builds_and_refuses_off_port_plain_and_ip_destinations`); no test starts a service that way. The feature also builds the fake Slack, whose committed server key (the CA's key was discarded)
   lets anyone impersonate the fixture's Slack hosts to a client that trusts the fixture CA, so it is enabled by
   dev-dependencies only (`sot-backend`'s), and three things hold it out of a shipped binary. First, `lib.rs` is a
   `compile_error!` when the feature is on and `debug_assertions` is off, so any release-profile build with it fails
   whatever the flag or manifest spelling. Second, `scripts/tests/release-features-check.sh` resolves `--workspace` over
   the normal and build edges for the release targets (one resolution covers `-p sot-frontend`, `-p sot-backend -p
   sot-capsule` and the macOS and Windows `--workspace` builds, since resolver 2 gives a package in a subset of the
   workspace a subset of its features) and fails, naming the feature and the crates that enable it, when sot-chat's
   `test-support` is in it, whether set on a normal dependency of any crate (also under a target table), in `default`, or by
   a member only `--workspace` builds; it runs in every gate run with each of those faults put in (`--prove`), and needs the
   release targets' packages in the depot (`cargo fetch --locked` once). Third: `--prove` also runs a release-profile check of `sot-chat` with the feature, which must fail with that compile error; deleting the `compile_error!` turns `--prove` red. Source builds by users (`launch-sot.sh`, `sot-freshness.ps1`) are `--release`, so the compile error covers them; a release-profile test build is refused by design. Limits, resting on review: a debug-profile build with the feature turned on by a command-line flag, a release profile that turns debug assertions on, a
   re-export of `for_test` or the fake outside its cfg, and code written to include the fixture's files from outside the
   gated module.
7. **An operation has one owner-held deadline and bounded admission.** `Request` carries its deadline; `send` runs the
   operation as a task that holds the deadline and the permit, so connection, headers, redirects and body share one
   bound and a caller that stops waiting neither frees the permit nor cancels a mutation already sent. Sixteen
   operations are admitted; the seventeenth is `Busy` at once. Limit: the permit returns only when the task ends
   (the deadline at the latest); a name lookup that outlives it keeps its blocking thread until the resolver returns.
8. **A response body is bounded while it streams.** At most 1 MiB, counted as chunks arrive, whatever `Content-Length`
   claims (a gigabyte claimed over a two MiB body, an endless chunkless body and one byte over the bound are `TooLarge`;
   a body of exactly the bound passes; a short claim ends the body at the claim).
9. **One `send` is one request.** The library's retry is set to never, HTTP/1 has no protocol retry, and no idle
   connection is kept, so a request is never re-sent on a stale pooled connection; a mutation the server accepted and
   then dropped is `Protocol`, with the server having seen it once.
10. **Failures are fixed codes and nothing formats a secret or a private URL.** `HttpError` is a code (`busy`, `timeout`,
   `destination`, `redirect`, `tls`, `connect`, `protocol`, `too_large`, `invalid`) with a fixed explanation and no
   payload; library errors are classified and dropped. `Secret` formats as a mask; `Request` and `Response` print no URL,
   header or body. The crate logs nothing. Limit: C2's proof covers errors, `Debug` and `Display` values and the
   request and response types; the mail and wire sinks, and the daemon's log masking, are later landings'.

11. **History pages are exact, complete or an error.** `HistoryQuery` and `Bounds` are validated when built: a bound is
   a wire `Ts` (exactly six fraction digits) or `BadBound`, before anything is sent; `before` and `until` cannot both be
   given; the limit is 1 to 20. Bounds map `after` to `oldest`, `before` to `latest` with `inclusive=false`, `until` to
   `latest` with `inclusive=true`; `include_all_metadata=true` is always sent. A `Continuation` wraps Slack's cursor with
   its workspace, channel, thread, bounds and limit, is at most 1 KiB (`TooLarge`), and under any other query, or if not
   made by this client, is `Altered`, refused unsent. `has_more` with no cursor is `Missing`, the cursor just sent
   coming back is `Repeated`, a missing `messages` or `has_more`, more than the limit (plus the thread parent: 21), an
   unrepresentable message, or a non-`ok` answer fails the whole page (no partial page); `is_limited` is `Page.limited`.
   The client rechecks every message against the bounds (the parent is exempt). Limit: a continuation is not
   authenticated, only matched to its query; one unrepresentable message fails its page, which the caller treats as a gap.
12. **A post is submitted once.** `Api::post` sends one `chat.postMessage` with the custom `username` (`<name> (<kind> via
   Ship of Tools)`) and `sot_chat_post` metadata (kind, name), so `normalize` recovers the origin the same way over
   history; there is no text prefix. `Refused` is Slack's definite rejection (a non-5xx status, or `ok: false` with a
   code that is not transient) or a request that never left (`busy`, `invalid`, `destination`); `Limited` is a 429 or
   `ratelimited`; every other case without a valid acknowledgement (transport error, timeout, 5xx, unparsable or `ok`-less
   body, transient code, `ok` with no `ts`) is `Unknown`. Nothing resends, and a caller that stops waiting does not
   cancel or repeat the request. Input a post cannot carry (id, name, text over 8 KiB, thread `ts`) is `Err` unsent. Every message text this crate sends is plain text made Slack's by one private function (`escape_text` after `post_text_fits`, wire's), the only way a request body gets a `text`; the 8 KiB bound is on the escaped size, and no post can mention a person or ping a channel until a structured field adds one. The daemon's hello says so (`HelloRes.posts_plain_text`).
13. **A retry header paces reads, never repeats a mutation.** The `Retry-After` header, in any case, in whole seconds
   (capped at one hour), makes every read refuse as `Limited` without a request until it passes; a missing or invalid
   header is an assumed 30 s (`assumed: true`). A post that is limited is returned with its wait and is not gated by the
   reads' wait.
14. **Every operation has a stated deadline.** Reads 15 s, a post 30 s (it is not resent, so an early give-up only
   makes `Unknown`), all of a page's name lookups together 10 s; each through `Http`'s owner-held deadline.
15. **Slack's text reaches no caller.** `ApiError` is a fixed code; Slack's `error` is carried only when it is
   `[a-z0-9_.]{1,64}`, else `unknown_error`; bodies and URLs are dropped; `auth.test` for another workspace is
   `WrongWorkspace`; history without the bot identity is `NoIdentity`, unsent.
16. **A name lookup that fails shows the id and is not remembered; the cache is bounded.** `author_name` and
   `fill_names` take `display_name`, then `real_name`, then `name`; only a success is cached, at most 1,024 entries,
   the oldest leaving first. Limit: a success is never refreshed within a process; a lookup that outlives the 10 s
   bound keeps its `Http` permit to that operation's own 15 s deadline.

17. **Each envelope is acknowledged, by its id, before any event work.** The reader writes `{"envelope_id": <id>}`
   as the first thing it does with a frame that carries one (an id over 128 bytes is not echoed and is a gap), whatever the
   frame's type, before normalization or the queue; the queue push never waits, so a consumer stalled on a name or file
   lookup, or a full queue, cannot delay an acknowledgement. Limit: a write that does not finish in 5 s retires the socket.
18. **One socket at a time, and a replacement waits for retirement.** One task owns the listener and its one socket. It
   opens the next socket only after the last is retired: `warning`, `refresh_requested`, any `disconnect`, a peer close, a
   silent close (no close frame), 45 s without a received frame (the clock restarts on a frame received, never on our own ping, every 30 s) and an
   acknowledgement that cannot be written all end the socket the same way. Retirement is a close the peer must answer within
   2 s (Slack's observed behavior: a peer that never answers blocks a read forever), then the connection is dropped. Reconnect
   is paced: 1 s, doubling to 30 s while attempts fail, reset by a `hello`, so a refusing Slack is not hammered and a healthy
   refresh is back in a second. `stop` sets the signal, the task retires the socket and ends, and `stop` joins it within 5 s
   (then aborts it); no connect starts after the signal. Limits: a process stalled for about the idle bound retires its link even when frames sit unread (the idle timer is polled ahead of the socket); the `Reconnect` gap and the history pull cover it. Nothing runs after `stop` except one request: a stop
   during `apps.connections.open` drops the waiter, and `Http::send`'s owner-held deadline (guarantee 7) lets that request,
   with its app token and permit, run on for up to 15 s. Slack's `Retry-After` on `apps.connections.open` is not read (the
   backoff paces it). Measured on a live Slack app (2026-11-06): on a quiet socket Slack pings about every 10.2 s (intervals
   10.12 to 10.31 s, a text payload), so no healthy gap between received frames passed 10.3 s; it answered all 19 of our
   pings, median 0.059 s; with nothing sent from our side it dropped the link at 45, 51 and 58 s as a bare end of
   stream, no close and no disconnect frame, and with replies going out the link held 600 s. The 45 s idle bound is
   about four of Slack's intervals and where Slack itself gives up on a silent peer. Every Slack ping is answered with
   a pong carrying its payload: the websocket library sends it at the reader's next poll, which follows any frame at
   once. The queue push never waits, so a stalled consumer or a full queue cannot hold a pong; only a write that cannot
   finish can, for at most its 5 s bound, after which the socket is retired. Not proven: whether answering alone
   keeps the link without our 30 s ping, which stays. A wrong value fails safe, as a reconnect, a `Reconnect` gap and a
   history pull, never a lost event. A `warning` retires the socket at once, by design (one socket, never overlapping).
19. **Nothing lost is silent.** A pending event is dropped, acknowledged, when 256 are pending or their frames pass 8 MiB
   (an event costs its envelope's size); an event that cannot be read or represented (bad JSON, no `payload.event`, an
   `Unrepresentable` result, a binary frame, a message over the bound) and a socket that ends, whether or not it said hello, each latch a `Gap`
   (`Overflow`, `Failed`, `Reconnect`); every `hello`, the first included, latches `Reconnect` too, so events Slack sent
   before a socket was live are never covered by a reconciliation that began before it. `Unsupported` subtypes and non-message events are neither. A gap stays until
   `Socket::reconciled(gap)` is given the value read before a complete reconciliation began; it clears only when nothing
   was lost since (every latch is a new sequence number, process-wide), so a loss during a reconciliation survives it, whether it is an overflow,
   a failed event or a lost socket.
20. **A websocket obeys the request rules and its own bounds.** The URL from `apps.connections.open` passes the guarantee-5
   predicate with scheme `wss` (userinfo, another port, an IP, a foreign or lookalike host, `ws` and `https` are
   `destination`, before any dial), connects directly (no proxy) and runs through `Http`'s one rustls configuration. A
   message is at most 1 MiB, by the library's frame and message limits (a larger one is `Capacity`, a gap and a new socket,
   never read whole). The connection and handshake share a 15 s deadline. The app token rides only the
   `apps.connections.open` request, which is a request under guarantees 5 to 10; the private URL and the ticket reach no
   error, status or format value (`SocketError` is a fixed code: `Http(code)`, `Rejected`, `Malformed`, `BadRequest`;
   Slack's text is dropped). A failed open is `Status.error`, paced and retried. Limit: C14's lock and second service
   are guarantee 22; its load half is guarantee 21.

21. **The configuration is read one way, refused with a stated reason, and shows no token.** `chat.toml` (the one
   workspace id, `channels`, `[[mapping]]` entries of `folder` and `channel`, an optional `mail_after` that is a `Ts`) and
   `chat-secrets.toml` (`workspace`, `bot_token`, `app_token`) are in platform's config folder (`config_dir`); only the
   named keys are accepted. Each file is read by `read_file` alone, which reads through platform's `read_regular`: a regular file (checked on the open handle; on Unix the open is non-blocking, so a planted FIFO returns at once, and a device or
   directory is refused; Windows applies no such flag and has no such test) of at most 64 KiB (`MAX_FILE`), text,
   read to at most one byte over the cap. The cap's job is to end a stall and bound the work a planted file causes (the
   channel check is quadratic and each mapping asks rows), not to be large. A second workspace (a list, a table, a mapping
   or the secrets naming another) and a repeated channel are refused at load. The bot token starts `xoxb-` and the app
   token `xapp-`, then letters, digits and hyphens, 512 bytes at most (a character platform's masker ends a token at, such as a quote or a parenthesis, is refused, so no tail of an admitted token prints in clear): a swap or equal tokens are refused, and every admitted token
   is a form platform's log masker knows. Mappings are admitted against supplied row facts (`Rows`: a folder's canonical
   spelling and whether it is the inert anchor; chat resolves no row): an unknown folder, the inert anchor, a channel not
   listed, two entries for one canonical folder (an alias, or the same pair twice) and one channel for two folders are
   refused (M1). `resolve(config, folder)` matches the canonical folder exactly and answers `Mapped` (delivering only
   with a `mail_after`), `Unmapped` or `Invalid(code)` (missing or malformed), each with a fixed `explain()`, and the
   daemon's own `NotHub` (M2: the daemon decided it is not the hub before any load, because another daemon on a shared
   home has other rows and would read every mapping as unknown). A refusal is a `ConfigError` code: a
   mapping refusal carries the mapping's ordinal and a parse failure its line, and no key or value from either file is
   quoted; the library's parse errors, which quote the source, are never formatted. Tokens are `Secret`s (guarantee 10's
   mask). `Config::load` opens `chat.toml` only; `Credentials::read` is the one call that opens `chat-secrets.toml`.
   **The caller (`rust/backend/src/chat.rs`, `Chat::begin`, C1).** In order, and only as the hub: topology's current
   `sot_topology::load_in` must parse and name this computer (`SOT_SELF_HOST` or its host name) as its explicit hub, so a
   missing or invalid topology, a computer with no name and another hub are all `NotHub` (`comm_topology_hub` accepts a
   missing file and `TopologyStore::refresh` keeps an earlier parse; neither is used); then `Config::load` against the
   daemon's rows through one canonicalizer (an absolute folder, resolved through symlinks, `.`, `..` and a trailing
   slash, de-verbatimed and in the system's letter case on Windows; a relative spelling or a folder that does not exist
   is unknown); then `Credentials::read`; then the lock folder (`<daemon state>/chat`, private); then registration with
   lifecycle's `retire::participants()`, made with the phase lock already held and kept until the start task is in
   place, so a retirement that reaches `request_stop` after the registration finds the start and aborts it (pinned by
   `a_retirement_reaching_request_stop_between_register_and_starting_finds_the_start`, through a test-only hook in
   lifecycle's `Registry`, which pins the lock order; the refusal of a registration once retirement began, and then no
   start, is pinned by `retirement_that_began_first_refuses_the_start`); then the start on a task, so boot never waits on Slack. A non-hub opens no chat file and
   registers nothing; a hub with no `chat.toml` is off (`Refused(Missing)`) and writes nothing; a malformed one opens no
   secret file. Every open goes through the one `Reader` passed to `begin`, and `begin` is the only caller of
   `Config::load` and `Credentials::read`: the status, its words, `resolve`, a stop and a wait read memory. The hub fact
   is read once. A daemon that stops being the hub after its start (the topology changes or vanishes) keeps its running
   service until it ends and opens no file again; a reload does not exist, and a start after a stop is a second `begin`
   (a restart of the daemon in practice). A failed start is final until the daemon restarts. **M3, `Chat::admit`, `Chat::admit_unwatch` and
   `Chat::resolve`.** `Chat::service` is private, so a call that reaches Slack or a file gets the service only from
   `admit`, which reads the registry's current entry for the row id at that moment (an exact id, never a slug) and
   refuses: no running service (not the hub, no or refused `chat.toml`, no secrets, a failed, starting, stopping or
   stopped service) as `unavailable`; a removed row, a row replaced under a new id, an id that never existed, an empty
   one and a folder that no longer exists as `stale_row`; the inert anchor, and a folder the file does not map, as
   `unmapped`; a folder mapped to another workspace or channel than the request expects, or an expected channel the
   file does not hold, as `stale_mapping`. A new row in the same folder does not inherit a request that named the old
   id, and a request naming the new id works. `admit_unwatch` hands out no service and needs only a running one: an
   unwatch calls nothing, and refusing it for a stale row would leave the watch holding one of the bounded slots. The
   mapping is the configuration loaded at start. Tests: `stale_row_or_mapping_cannot_post_or_fetch` and
   `no_action_is_admitted_unless_the_service_runs` prove a refusal by no service being handed out, and the backend's `server/chat_ops_tests.rs` proves it through a connection by the fake's record holding no history, replies, `chat.postMessage` or `users.info` request (the listener's own `auth.test` and `apps.connections.open` are in the record and are not counted); that `service` is private rests on its visibility and review, no test can see it (they are a
   child module). `resolve` asks the hub fact first, then the row, then the file: `NotHub` for a non-hub whatever the
   id, `stale_row` for a row that is gone, `Unmapped` for the anchor; otherwise the file's mapping in every service
   status, with `delivers` only while the service runs (starting, failed, secrets missing, stopping and stopped show
   the mapping and do not deliver) and `Invalid` for a refused or missing `chat.toml`
   (`resolve_joins_the_mapping_with_the_row_and_the_service_status`,
   `resolve_asks_the_hub_fact_first_and_the_row_before_the_file`). Limit: the check is at admission and holds no rows
   lock across a Slack call, so a call already made, a history page's name lookups included, is not recalled when its
   row is removed afterwards. The request ops (`chat/ops.rs`) call `admit` on the blocking pool, since it canonicalizes on the shared home (rests on review: no test can make a folder lookup slow). `chat.watch` (`chat/ops.rs`) and each snapshot attempt the watch owner makes (`chat/watch.rs`) are admitted again through `admit` when made, so a watch whose row went away is refused and its slot released; `chat.unwatch` calls `admit_unwatch`. The listener's own calls (`auth.test` at start, `apps.connections.open` for each socket) are Slack traffic made with no admission: they name no row and act for no request, so M3 governs only what a request names; the listener makes no name lookups. Rows made after the start are not seen: a mapping to a folder no
   row held then makes `chat.toml` refused (`UnknownFolder`) until the daemon restarts. A second daemon of the account
   on a home that names the same hub reads the files like the first and is `Held` by C14's lock. Retirement: lifecycle
   closes admission (the chat has none yet: a handler's `admit` is `unavailable` once the stop begins), then
   `request_stop` spawns `Service::stop(500 ms)` on the daemon's runtime (a start still under way is aborted;
   `retiring_a_start_under_way_drops_its_future`, and `a_start_that_finishes_as_retirement_arrives_is_stopped_at_once`
   for a service made as retirement arrives, each failing at a 10 s bound with a message naming the rule), and
   `wait_retired` waits for the stopped state; lifecycle's 1 s bound cancels no future, so a stop that outlasts it is
   reported incomplete and the daemon ends regardless. Limits: the Windows spellings (drive-letter case, separators)
   rest on canonicalization and are compiled, not run, here; the running-listener paths are proved with a test
   listener, since `Service::start` reaches Slack only (the backend's connection tests reach the fake through `Service::start_at`; the service's own stop is
   guarantee 22's); a file's permissions are not checked here; the attachment bound is B4's.
22. **One workspace has one listener, and its lock lives exactly as long as the listener.** `Service::start(config,
   credentials, http, dir)` takes the lock first, then reads `auth.test`, then opens the socket; a second service for the
   workspace is `ServiceError::Held` before any socket opens or any call is made. The lock is platform's `lock_supervisor`
   (a kernel `flock`, close-on-exec) on `<dir>/chat-<workspace>.lock`, a name built from the validated workspace id alone,
   holding no secret; `dir` is a private folder the caller supplies and owns (not checked here). The lock is moved into the
   listener's task (`Socket::launch`'s `held` argument), so it is released when the task's future is dropped, after its last poll: at a
   normal end, at a `stop` that timed out and aborted (the abort is awaited first), and at a drop without `stop` (the
   abort, not an await). A killed holder releases it with its descriptor, a child born later does not inherit it, and
   `start` after `stop` in one process is admitted. A token whose `auth.test` names another workspace is
   `Api(wrong_workspace)` before serving, and the lock is released. `stop(within)` is bounded and final: the signal, a join
   within `within`, then abort and await; a second call returns at once, and `api()` is `Stopped` and every handle already taken answers `Closed` (the API itself is closed, before the socket is
   signaled) from the moment a stop begins; only the call that began the stop waits for the listener, any other returns at once;
   a request already in flight (including `apps.connections.open`, whose ticket nobody connects with) runs to its own deadline. `status()` is `Running { socket, gap }` (the socket's `Status`; `gap` is true after any hello until `Watches::reconcile` clears it through `Service::reconciled`; `Service::gap` is a status read; `Watches` takes the events, answers `loaded`, clears through `reconcile` and waits, and all of them answer nothing once a stop has begun; a `wait` already in flight is woken by the stop and answers `Stopped`),
   `Stopping` or `Stopped`. A service holds four things: the workspace lock, its phase, the API's closed state (set when a stop begins, never cleared, seen by every handle), and the claim that makes `Service::watches` give one `Watches`: free, held while that `Watches` lives, and ended when a stop begins (or the service is dropped), which is never undone, so a stopped service gives none and its `Watches` reads it. A stop whose caller stops waiting (its future is dropped, as lifecycle's 1 s retirement bound does) leaves that service at `Stopping` for good, not `Stopped`: every handle answers `Closed` and the task is aborted, so the lock frees when the runtime drops the task and a `start` right after may wait platform's short retry; nothing may poll for `Stopped` after cancelling. A service refused or never started has no value, its state is `start`'s error. The error codes are
   the existing ones plus `held`, `lock`, `stopped`, and `closed`, which a handle answers once its service has begun to stop. Limits: the lab's computers share a home over NFSv4.2, where `flock` is
   a byte-range lock on the server: measured, a lock held on one computer refused `flock -n` on another and freed when the
   holder ended; a holder killed on one computer, a lost computer (lease expiry) and a mount that does not support locks
   are not measured, and Windows is `LockFileEx` with no cross-computer claim. A path outside the shared home (a
   per-computer `dir`) gives per-computer exclusion only. `auth.test` reports no app id, so a duplicate app id across
   different workspace ids, and an app token that belongs to another app than the bot token, are not detected here. A
   mutation or name lookup begun before `stop` runs to its own deadline (guarantees 7, 12, 16). The service's `stop` bound
   is the caller's; the daemon's retirement of in-process services allows 1 s (`lifecycle`), and a `within` over that is
   reported incomplete there, with the lock released by the process's end.

23. **Watching cannot lose the snapshot/event overlap silently (M5).** `Watches` is a model with no task and no call,
   one per `Service`, made by `Service::watches`, which gives none while another is alive (a second would find the events already taken by the first and hold stale content as `Complete`), and a `Watches` reads only the events of the service that made it; its caller
   (the wire and daemon landings; until then the tests) feeds it the socket's events (`pump(&service)`, which takes
   and applies them) and each snapshot's answer. A channel is registered (`watch`) before its snapshot is begun (`begin`,
   which returns a ticket), so no event falls between them. While the snapshot is pending, live changes go into an
   overlay of at most 128, applied after the snapshot in order: an edit or delete seen live wins over the snapshot's
   older content, a message the snapshot lacks is kept, and a new message never replaces one held. An edit of a message
   not held is never added (it rests on the socket delivering a message's new event before its edits, which B2e
   checks against Slack; it is older than the page; a newer one missed would have been a gap), and a delete of one
   never held is a no-op. An overlay past its bound is dropped and its snapshot refused (`Resync`, the channel `Needed`,
   nothing kept from it). Every gap, every hello included, makes every complete watch `Needed`, and a snapshot is
   accepted only when no gap newer than the one known when it began has been seen, so a gap latched while one is
   pending, or a second during a resync, refuses it. A gap's identity is a process-wide sequence number (`Socket`),
   so no two gaps are equal and the model's order by number is total. A snapshot that
   fails or whose service stopped (`failed`) leaves a new one owed. An answer whose ticket is not the channel's pending
   one (unwatched, watched again, begun again) is `Stale` and changes nothing. The socket's gap is cleared
   (`reconcile(&service)`) only when every watch that exists is `Complete`, hence resynced from a
   snapshot begun after that gap was read, and the socket refuses it when anything was lost since; with nothing watched
   it clears at once. The set is the watches today, and B3's mail joins it. One total order on timestamps
   (`normalize::ts_key`, also `Bounds`'): numeric on the validated form, so `9.000000` precedes `10.000000` and
   `007.000000` is `7.000000`, one message. Bounds: 32 channels, 100 messages a channel (the oldest leave), 128 overlay
   changes, a channel id of at most 64 characters; the worst case is 32 x (100 + 128) x 32 KiB, about 228 MiB. Limits:
   "complete" is the latest page of history (at most 20 messages, `Page`) plus every change since, never older
   messages; the model holds top-level messages of its channel only, so a thread reply, a thread broadcast included, is
   neither kept from a snapshot nor applied live (a parent's reply count is not tracked); a snapshot nobody answers
   leaves its channel `Pending`, so the caller answers every ticket with `loaded` or `failed`; one watch that can never
   complete (a channel the bot cannot read) keeps the gap, so the caller unwatches it; a gap newer than the model's,
   not yet synced, is seen at the next pump, which the gap's own wake calls.
   **A snapshot's order is not the caller's.** `Watches::begin(channel, &service)` takes the events pending in the
   socket and the gap in force under one lock (`Socket::take`), syncs the gap, applies the events to the channels and
   only then marks the channel pending, so an event queued before the snapshot never wins over it (applied after
   `begin` it would, and the model cannot tell a queued old event from a live one). `loaded(.., &service)` reads
   the service's gap as the answer arrives and syncs it, so a loss after the take, a burst included, refuses the snapshot (`Resync`) and
   the gap does not clear; the loss is one before the answer arrives, and a later one is seen at the next pump (the wake
   it owes) and leaves `reconcile` refused. The model's gap moves forward only, by the process-wide number: a value read
   before a newer one was seen, handed in after, is ignored, so no ordering of tasks on one `Watches` rolls it back. `take` is the one way events leave the socket in the product (`Socket::next` is
   test-only), and it is crate-private with `Taken`, taking the `Watches` that asks: the callers are `Watches::pump` and `begin`, which take and apply
   in one call under the `Watches`' exclusive borrow, so a `Taken` is never held across a lock or an await, and no two
   readers can apply an older event after a newer one. `Watches::wait(&service, within)` is the pump's wake: `Wake::Look` (an event queued or a gap latched, or anything still queued; may be spurious; nothing leaves) means pump, `Idle` a bound with nothing to take, and `Stopped` that the service is no longer running (a wait in flight included, and every wait after) or is not this `Watches`' own, so a loop ends there and never spins. It takes `&mut self`, so with one `Watches` per service one wait is ever in flight and no second waiter can take the wake; a daemon that shares the `Watches` behind a lock must not hold it across the wait, so one task owns it (`sotd`'s is `chat/watch.rs`'s owner) and `select!`s the wait against its commands (cancelling a wait loses no wake). A stop is read from the claim, not from a call: once the service's stop has begun, `state` reports every channel `Needed` whatever the `Watches` is called against or never called against, `begin` gives no ticket, `loaded` is `Stale` and `reconcile` false, so a kept `Watches` never serves a stopped service's content as complete (`messages` still shows what was held). `loaded` and `reconcile` take the service, so a `Watches` answers with and clears only its own service's gap (`loaded` on another's is `Stale`; gap numbers are process-wide, so another's is never cleared). The design rests on Slack's history, read after the take, already holding every event the socket delivered
   before it (B2e checks this against Slack): a queued event is discarded for the snapshot, and a lagging history would
   drop an edit.

The timestamp rule (exactly `<seconds>.<six digits>`) is wire's `Ts`; this crate builds only `Ts` values, so a record
never holds another form.

## Owns
`normalize.rs`: `normalize`, `Context`, `Normalized`, `Unrepresentable`, `ts_key`; it re-exports wire's record bounds (`MAX_RECORD_BYTES`, `MAX_TEXT`, `MAX_FILES`, `MAX_ID`, `MAX_NAME`, `MAX_FILENAME`, `MAX_MIME`) and the id grammar `is_id` (as `valid_id`), which wire owns. `http.rs`: `Http`,
`Request`, `Response`, `Secret`, `HttpError`, `is_slack_host` and the bound constants (`MAX_OPERATIONS`, `MAX_BODY`,
`MAX_REDIRECTS`). `api.rs`: `Api`, `HistoryQuery`, `Bounds`, `Upper`, `Continuation`,
`Page`, `PostKind`, `PostOutcome`, `Identity`, `ApiError` and the deadline constants and `MAX_NAMES`, `ASSUMED_RETRY`; `MAX_PAGE`, `MAX_CURSOR` and `MAX_RETRY` (wire's `MAX_RETRY_SECS`) are wire's, re-exported. `socket.rs`: `Socket`,
`Gap`, `GapCause`, `Taken`, `Status`, `SocketError` and the bound constants (`MAX_PENDING`, `MAX_PENDING_BYTES`, `MAX_MESSAGE`);
`http.rs` also holds `Http::websocket`, the one way to a websocket. `service.rs`: `Service`, `Status`, `ServiceError`. `watch.rs`: `Watches`, `State`, `Loaded`, `Wake`, `WatchError` and the bound constants (`MAX_CHANNELS`, which wire's `MAX_WATCHED` repeats and `chat_and_wire_agree_on_the_numbers_they_share` holds equal, `MAX_KEPT`, `MAX_OVERLAY`). `config.rs`: `Config`, `Credentials`, `Resolution`, `ConfigError`, `Folder`, `resolve`, `read_file`, `MAX_FILE`, `config_dir`. `fake_slack/` (under `cfg(test)` or `test-support`): the scripted fake Slack
and its websocket peer (`ws.rs`).

## Calls in
`sot_protocol::chat::{ChatRecord, ChatAuthor, ChatFile, Origin, Ts, is_id, MAX_*}`, `sot_protocol::ops::{MAX_PAGE, MAX_CURSOR, MAX_RETRY_SECS}` (and, in a test, `MAX_WATCHED`), `serde_json::Value`; reqwest, rustls and
webpki-roots and tokio-tungstenite inside `http.rs` (`socket.rs` uses the library's `Message` and `Error` types; `api.rs` uses
`reqwest::Url` to build query strings). Dev only: `sot_platform::test_isolated` (`sot-platform` with `test-support`).

## Does not
Open a socket except through `Http::websocket`, call a file method outside `config.rs`, hold a credential at rest, read a token from the environment, file mail, or resolve a row (it maps folders to channels from supplied facts). Define the
record's representation (wire's). Offer another way to the network, a retry helper or a generic outbound HTTP type.

## Dependencies (the web stack)
Pinned as measured on the branch `stack/web-tls`: `reqwest =0.13.5` (no default features; `rustls-no-provider`, `json`,
`stream`), `rustls =0.23.46` (no default features; `ring`, `std`, `tls12`), `webpki-roots =1.0.11`, all in the workspace
table; `tokio` from the workspace; `tokio-tungstenite =0.30.0` (no default features; `connect`, `rustls-tls-webpki-roots`, the
pin measured on that branch) and `futures-util 0.3` for the socket; dev, and optional under `test-support`: `tokio-rustls 0.26` (no default
features; `ring`, `tls12`) and `sot-platform` with `test-support`. The HTTP change added 37 `[[package]]` entries to
`Cargo.lock`, the socket 14 (`tungstenite`, `tokio-tungstenite`, `sha1` and its digest family, `data-encoding`, `rand`
0.10 and `webpki-roots` 0.26, which the websocket feature brings beside 1.0.11; its roots are never used, because
`Http::websocket` passes `Connector::Rustls` the one finished configuration). `sot-chat`'s own tree
(`cargo tree -p sot-chat --target all -e all`) holds no aws-lc, openssl-sys, cmake, pkg-config, vcpkg, bindgen or
native-tls (`wasm-bindgen` and `wit-bindgen`, on wasm targets, are other crates); of those crates the workspace lock
holds only pkg-config, through the frontend. The tree does hold reqwest's `rustls-platform-verifier` and its per-OS
readers (`rustls-native-certs`, `openssl-probe`, `schannel`, `security-framework`), compiled and never used, because
`http.rs` hands reqwest a finished rustls configuration. The one native build is `ring` through `cc` (a C compiler for the
target; the gate's `darwin-check`, on a Linux host with no Apple clang, gives `ring`'s build script the stand-in
compiler `scripts/tests/cc-stub.sh`, which writes empty objects because the check links nothing; hosted macOS builds
it for real). 
## The fake Slack
`fake_slack/mod.rs` (under `cfg(test)` or the `test-support` feature, which another crate's tests enable as a dev-dependency; guarantee 6 says what holds it out of a shipped binary) is an HTTPS server on loopback over real TLS (`tokio-rustls`), hand-written, scripted per
test (`Reply`: a status, raw bytes, a hang, a stall after bytes, an endless body, a dropped connection), recording each
request (with its path, query parameters and JSON body), connection and handshake failure. It flushes after every
write and its listener has a 4 KiB send buffer, so output left unflushed in the TLS session hangs on Linux as it did on
Windows (`chat_operations_keep_bounds_after_caller_drop`, `/exact`, red until the flush). Its listener and connection tasks belong to the `FakeSlack`: `shutdown`
joins them within five seconds and a drop aborts them. Its fixed certificates (a CA valid to 2126 whose key was discarded,
a CA-signed server certificate, a self-signed one, each back-dated to 2025-01-01 so a host with a slow clock accepts them)
and their generation are described in its header; `.gitleaks.toml`
allows its two `.key` files by exact path and no other file.

A request that asks to upgrade is answered with `Reply::socket`: the fake runs the selected library's server
handshake (`accept_async`) over its TLS, then a script drives the connection through a `WsPeer` (`send`, `recv`, `drain`,
which lets the library answer a close, and `ignore_until_gone`, which never reads a frame, so never answers a close). It
counts sockets open now, the most open at once and all accepted: a socket is open until its script ends. The logical
host `wss-primary.slack.com` is in the certificate and the resolver, like the others.

## Tests
Paths are under `rust/chat/src/`.

| # | guarantee | test |
|---|---|---|
| 1 | result for every shape | `normalize_tests.rs`: `normalize_members_bots_edits_deletes_and_file_stubs`, `hostile_json_gives_an_explicit_result` |
| 2 | own posts by supplied identity only | `normalize_tests.rs`: `normalize_members_bots_edits_deletes_and_file_stubs` (member prefix, member username, other bot), `with_no_identity_supplied_a_member_with_a_forged_prefix_is_a_member`, `a_prefix_in_an_own_posts_text_is_only_text`, `an_unreadable_own_post_keeps_its_origin_and_an_unreadable_reply_its_thread` |
| 3 | bounds | `normalize_tests.rs`: `text_is_cut_on_a_character_boundary_and_marked`, `names_filenames_and_mime_are_cut_and_marked`, `only_eight_files_are_summarized_and_the_rest_counted`, `an_id_over_64_bytes_or_not_ascii_is_not_carried`, `a_record_never_exceeds_32_kib_of_json`, `names_lose_control_characters`, `a_normalized_record_round_trips_through_the_wire` |
| 4 | attachment only | `normalize_tests.rs`: `an_attachment_only_message_keeps_its_files_and_has_no_text` |
| 5 | Slack destinations and authorization | `http_tests.rs`: `private_redirect_keeps_slack_auth_and_refuses_foreign_destinations`, `slack_hosts_are_slack_com_and_its_dot_boundary_subdomains`, `a_mutation_is_sent_once_and_never_followed_to_a_redirect`, `an_ambient_proxy_variable_is_ignored` (the test re-runs itself through `sot_platform::test_isolated` as a child with every proxy variable naming a dead address and expects a direct request; the helper fails the parent when the body was not entered exactly once or exited non-zero), `the_product_client_builds_and_refuses_off_port_plain_and_ip_destinations` (no network) |
| 6 | verified server, and no test root in a shipped binary | `scripts/tests/release-features-check.sh --prove` (the gate runs it: green for the real tree and for the feature on a dev-dependency alone; red with the feature on a normal dependency, in `default`, on the frontend, in a target table, or on a member only `--workspace` builds); the compile error is shown by a release build of `sot-chat` with the feature failing and a test build passing, and `sot-backend`'s `the_exported_fake_slack_answers_one_scripted_reply` (the export works); `http_tests.rs`: `an_untrusted_certificate_fails_before_any_http`, `a_name_the_certificate_does_not_cover_fails_before_any_http` (each with a passing control; the second also the resolver's refusal) |
| 7, 8 | deadline, admission, body bound | `http_tests.rs`: `chat_operations_keep_bounds_after_caller_drop` |
| 9 | one send, one request | `http_tests.rs`: `one_send_is_one_request_even_when_the_server_closes_mid_response` |
| 10 | no secret in a formatted value | `http_tests.rs`: `chat_secrets_stay_inside_daemon_boundaries` |
| 11 | exact history pages | `api_tests.rs`: `history_pages_exact_query_without_silent_completion`, `a_bound_without_exactly_six_fraction_digits_is_refused_before_sending` |
| 12 | one post, origin kept | `api_tests.rs`: `lost_post_answer_is_unknown_and_never_resent`, `a_post_carries_its_author_and_every_unconfirmed_answer_is_unknown`, `a_post_read_back_through_history_keeps_its_origin`, `a_message_text_is_sent_escaped_and_bounded_escaped` |
| 13 | retry header | `api_tests.rs`: `retry_header_paces_reads_and_preserves_mutation_draft` |
| 14 | deadlines | `api_tests.rs`: `every_operation_ends_at_its_deadline`, `name_lookups_together_end_at_the_names_deadline` |
| 15 | no raw Slack text | `api_tests.rs`: `slack_text_and_identity_failures_reach_no_caller_raw` |
| 16 | names | `api_tests.rs`: `names_fall_back_to_ids_without_unbounded_cache` |
| 17 | ack first (C11) | `socket_tests.rs`: `socket_ack_precedes_slow_api_and_dispatch` (the fake sends each envelope only after the previous one's acknowledgement while the dispatcher is stalled on a lookup that never answers), `a_write_that_cannot_finish_retires_the_socket_within_the_write_bound` (a peer that never reads fills the buffers; the socket is dropped, not held, with idle far past the test) |
| 18 | one socket (C12) | `socket_tests.rs`: `disconnect_refresh_and_silent_close_keep_one_socket` (warning, refresh, silent close, a peer that ignores close, endless refreshes; peak open 1; stop bounded and final), `a_peer_that_ignores_close_is_forced_down_within_the_bound`, `the_product_numbers_are_the_measured_ones` (all eight product timings), `a_silent_socket_is_replaced_after_the_idle_bound` (ping shorter than idle, as in the product; replaced no sooner than idle and within a stated bound), `a_link_that_answers_is_kept_past_the_idle_bound`, `a_chatty_peer_that_never_answers_keeps_its_link`, `a_peer_that_only_pings_keeps_its_link` (a peer ping alone resets the idle clock), `every_slack_ping_is_answered_with_its_payload_while_the_consumer_stalls_and_the_queue_is_full` (a pong frame with the payload, in both states), `a_bare_end_of_stream_is_replaced_within_the_pacing_and_latches_a_gap`, `reconnects_are_paced_after_a_refusal_and_after_a_hello`, `stop_during_the_reconnect_pause_does_not_wait_for_it` (every test here that holds a link or a measured duration against a bound runs under `steady`: an attempt that fails beside a ticker that saw a pause of that bound's margin is run again, up to five times; a failure on an unpaused host fails the test and names the longest tick) |
| 19 | gaps (C13) | `socket_tests.rs`: `a_gap_opened_by_an_outage_survives_a_reconciliation_made_during_it`, `a_failed_event_or_a_lost_socket_during_a_reconciliation_keeps_the_gap`, `a_reconciliation_begun_before_a_hello_is_refused_by_it`, `overflow_latches_gap_without_blocking_ack` (a failed event; 300 envelopes over the count with every one acknowledged; a loss during a reconciliation survives it; ten 900,000-byte events over the byte budget), `an_events_message_keeps_its_origin_through_the_socket` |
| 20 | websocket bounds and destinations | `socket_tests.rs`: `a_message_over_one_mib_is_refused_before_it_is_read_whole` (one byte over is not delivered, exactly the bound is), `a_foreign_websocket_url_fails_visibly_and_leaks_nothing` (also eight spellings refused by `Http::websocket`) |
| 21 | configuration (M1, M2, C14's load half) | `config_tests.rs`: `mapping_refuses_aliases_duplicates_and_inert_anchor`, `resolve_explains_unmapped_invalid_and_disabled_delivery`, `a_second_workspace_in_the_file_or_a_mapping_is_refused_at_load`, `every_refusal_has_an_input_a_code_a_place_and_its_own_explanation` (a row per `ConfigError` variant, the boundaries, the two file names), `a_token_prints_no_bytes_and_a_load_error_quotes_no_secret_value`, `loading_the_configuration_opens_no_secret_file` (an observing reader), `the_real_read_takes_a_regular_file_up_to_the_cap_and_nothing_else`, `a_planted_fifo_or_device_is_refused_without_a_wait` (the read runs on a thread with a 10 s guard that only a failing read reaches), `the_files_are_named_in_platforms_config_folder` |
| 21 (the caller, M3) | every chat action rechecks the row and the expected pair; `resolve` joins the status | `rust/backend/src/chat_tests.rs`: `stale_row_or_mapping_cannot_post_or_fetch`, `no_action_is_admitted_unless_the_service_runs`, `resolve_joins_the_mapping_with_the_row_and_the_service_status`, `resolve_asks_the_hub_fact_first_and_the_row_before_the_file` |
| 21 (the caller, C1, M2's non-hub answer) | only the hub opens a chat file; rows through the real canonicalizer; retirement | `rust/backend/src/chat_tests.rs`: `only_hub_reads_chat_credentials_from_platform_config` (a non-hub of three kinds and a hub that stops being the hub, through an observing reader), `a_hub_with_missing_or_refused_files_is_off_and_writes_nothing`, `a_non_hub_in_a_shared_home_answers_not_hub_where_a_hub_without_the_row_refuses`, `mappings_are_admitted_against_the_rows_canonical_folders`, `lifecycles_retirement_stops_the_running_service_within_its_bound`, `a_stop_that_does_not_finish_is_reported_incomplete_and_final`, `a_start_under_way_when_retirement_comes_is_ended_with_it`, `retirement_that_began_first_refuses_the_start`, `a_start_that_fails_leaves_the_chat_off_with_its_code_and_a_private_lock_folder_is_given`, `a_dropped_chat_lets_its_listener_go_and_a_start_after_a_stop_is_a_new_chat` |
| 22 | one listener (C14's running half) | `service_tests.rs`: `duplicate_workspace_or_app_has_one_listener` (a second service in this process: `Held`, no call, no socket; start after stop), `a_token_for_another_workspace_is_refused_before_serving`, `stop_is_bounded_final_and_repeatable_and_the_lock_waits_for_the_listener` (a peer that ignores close; the lock held while stopping, free after), `a_handle_kept_across_stop_is_refused_by_every_call`, `the_lock_is_dropped_with_the_task_before_stop_returns` (the abort is awaited), `a_lock_that_cannot_be_made_is_not_held_and_the_codes_are_fixed`, `a_second_concurrent_stop_returns_at_once_and_the_status_reaches_stopped`, `a_stop_before_the_socket_connected_returns`, `a_dropped_service_releases_the_lock_only_with_its_listener`, `a_killed_holder_leaves_the_workspace_free` (a second process; SIGKILL), `a_child_started_by_the_daemon_does_not_keep_the_lock` (Unix only; relies on platform's close-on-exec, which no chat mutation reaches) |
| 23 | watching (M5) | `watch_tests.rs`: `watch_snapshot_overlap_and_lag_resynchronize` (live edit, delete and add during a snapshot; a new message never replaces; the overlay at its bound and one past it), `a_gap_makes_every_watch_incomplete_until_a_later_snapshot_and_clears_only_then` (a gap pending, a second gap during a resync, a refused clear, the value given), `a_gap_with_nothing_watched_clears_and_a_watch_begun_after_it_needs_no_resync`, `events_nobody_keeps_and_answers_nobody_wants` (an unwatched channel, replies, an unwatch and a re-watch during a snapshot, a failed snapshot, an old page's edit and an unheld delete), `syncing_a_gap_already_known_keeps_a_complete_watch`, `a_snapshot_replaces_what_is_held_and_a_queued_event_never_wins_over_it`, `a_burst_after_the_events_were_taken_refuses_the_snapshot`, `a_gap_read_before_a_newer_one_was_seen_cannot_roll_the_model_back`, `no_interleaving_of_a_snapshot_a_pump_and_the_source_ends_complete_over_a_lost_event` (all 4,200 orders of a snapshot task (begin, fetch, read the gap, answer), a pump task (two pumps, a reconcile, then one more pump when `wait` says look) and the source, two starts, delivered or lost third edit, so 16,800 runs, each through a `Service` over a socket with no task, so the product's own take and wake run: a complete channel is never stale, a gap is never cleared over a stale model), `begin_applies_the_events_it_is_given_to_every_channel_whether_or_not_it_begins`, `a_snapshot_keeps_only_this_channels_top_level_messages`, `timestamps_order_numerically_on_validated_values`, `the_watch_is_bounded_in_channels_ids_and_messages`; `socket_tests.rs`: `take_hands_over_every_pending_event_and_the_gap_and_frees_the_budget`, `wait_wakes_on_a_gap_and_on_an_event_and_takes_nothing` (the event arrives while the wait waits); `service_tests.rs`: `the_watch_clears_the_services_gap_and_a_stopped_service_clears_nothing`, `a_stop_makes_every_watch_owed_a_snapshot_and_a_stranger_clears_nothing`, `a_wait_woken_while_the_stop_is_under_way_answers_false`, `a_dropped_service_ends_its_watch`, `a_stop_under_way_already_ends_the_watch` (a cancelled stop leaves `Stopping`), `loaded_reads_its_own_services_gap_and_a_stop_ends_what_was_pending_and_the_claim`, `a_pump_loop_beside_a_latched_gap_sees_it`, `wait_wakes_for_a_gap_and_an_event_sees_what_is_queued_and_ends_with_the_stop`, `a_wait_in_flight_ends_at_the_stop`; `watch_tests.rs` also: `pump_and_begin_apply_the_services_events_in_the_call_that_takes_them`, `a_service_has_one_watches_and_no_other_reads_its_events` (a stranger's take, answer and wait), `many_threads_asking_at_once_get_one_watches` (8 threads, 3,000 rounds) |

## Files
- `Cargo.toml`: the manifest; the dependencies are listed at the head of this page.
- `src/lib.rs`: the modules and re-exports.
- `src/normalize.rs`: the normalization. `src/config.rs`: the configuration; `src/config_tests.rs`: its tests.
- `src/http.rs`: the HTTP core and `Http::websocket`. `src/api.rs`: the Slack calls; `src/api_tests.rs`: their tests.
- `src/socket.rs`: the Socket Mode listener; `src/socket_tests.rs`: its tests.
- `src/service.rs`: the service; `src/service_tests.rs`: its tests.
- `src/watch.rs`: the watch model; `src/watch_tests.rs`: its tests.
- `src/fake_slack/`: the fake (tests and the `test-support` feature), its websocket peer and its certificates. `src/http_tests.rs`: the HTTP tests.

## Start here
`src/normalize.rs`, `normalize`; `src/http.rs`, `Http::send`; `src/api.rs`, `Api::history` and `Api::post`; `src/socket.rs`, `Worker::run` and `Worker::handle`.
