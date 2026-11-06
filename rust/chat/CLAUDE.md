# rust/chat: chat (contract)

The project chat's policy. This stage holds normalization (Slack's raw message JSON into the wire's one record,
`sot_protocol::chat::ChatRecord`), `http`, the one way out of the crate to the network, `api`, the Slack Web API calls
the first stage needs (`auth.test`, history and replies, `chat.postMessage`, `users.info`), and `socket`, the Socket Mode
listener, and `config`, the two configuration files. The crate (`sot-chat`) depends on `sot-protocol`, `sot-platform` (the config folder), `toml`, `serde_json` and the web stack recorded below. `sot-backend` depends on it and no daemon code names it yet, so `sotd` holds none of it. It owns no runtime state beyond `Http`'s client and admission count, `Api`'s
bot identity, read-pacing instant and name cache, and a `Socket`'s one task, queue and gap, and has no delivery. Its one file calls are `config`'s two reads (guarantee 21); it reads no token from the environment.

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
   filename and MIME); the record's size is held by halving the text until it fits, and once the text is gone the other bounds (ids, names, eight files, three timestamps) keep it near 8 KiB, far under the cap, so nothing else is cut. Names, filenames and MIME lose control characters and are trimmed of surrounding whitespace (`clean`), and nothing else is stripped: direction and other format characters stay, for the later display and mail rules. A
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
   path. `Http::for_test`, under `cfg(test)` alone, takes the fixture's CA as the only root and a resolver that maps the
   fixture's logical hosts to loopback and refuses every other name (`Connect`, no connection).
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
   cancel or repeat the request. Input a post cannot carry (id, name, text over 8 KiB, thread `ts`) is `Err` unsent.
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
   (then aborts it); no connect starts after the signal. Limits: nothing runs after `stop` except one request: a stop
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
   was lost since (every latch is a new sequence number), so a loss during a reconciliation survives it, whether it is an overflow,
   a failed event or a lost socket.
20. **A websocket obeys the request rules and its own bounds.** The URL from `apps.connections.open` passes the guarantee-5
   predicate with scheme `wss` (userinfo, another port, an IP, a foreign or lookalike host, `ws` and `https` are
   `destination`, before any dial), connects directly (no proxy) and runs through `Http`'s one rustls configuration. A
   message is at most 1 MiB, by the library's frame and message limits (a larger one is `Capacity`, a gap and a new socket,
   never read whole). The connection and handshake share a 15 s deadline. The app token rides only the
   `apps.connections.open` request, which is a request under guarantees 5 to 10; the private URL and the ticket reach no
   error, status or format value (`SocketError` is a fixed code: `Http(code)`, `Rejected`, `Malformed`, `BadRequest`;
   Slack's text is dropped). A failed open is `Status.error`, paced and retried. Limit: C14's lock and second service
   are the service landing's; its load half is guarantee 21.

21. **The configuration is read one way, refused with a stated reason, and shows no token.** `chat.toml` (the one
   workspace id, `channels`, `[[mapping]]` entries of `folder` and `channel`, an optional `mail_after` that is a `Ts`) and
   `chat-secrets.toml` (`workspace`, `bot_token`, `app_token`) are in platform's config folder (`config_dir`); only the
   named keys are accepted. A second workspace (a list, a table, a mapping or the secrets naming another), a repeated
   channel and equal tokens are refused at load. Mappings are admitted against supplied row facts (`Rows`: a folder's
   canonical spelling and whether it is the inert anchor; chat resolves no row): an unknown folder, the inert anchor, a
   channel not listed, two entries for one canonical folder (an alias, or the same pair twice) and one channel for two
   folders are refused (M1). `resolve(config, folder)` matches the canonical folder exactly and answers `Mapped` (delivering
   only with a `mail_after`), `Unmapped` or `Invalid(code)` (missing or malformed), each with a fixed `explain()` (M2;
   the non-hub answer comes with the hub landing). A refusal is a `ConfigError` code: no value from either file is quoted
   and the library's parse errors, which quote the source, are never formatted. Tokens are `Secret`s (guarantee 10's
   mask). `Config::load` opens `chat.toml` only; `Credentials::read` is the one call that opens `chat-secrets.toml`
   (the hub landing guards it). Limit: the file's permissions are not checked here, and the attachment bound is B4's.

The timestamp rule (exactly `<seconds>.<six digits>`) is wire's `Ts`; this crate builds only `Ts` values, so a record
never holds another form.

## Owns
`normalize.rs`: `normalize`, `Context`, `Normalized`, `Unrepresentable` and the bound constants. `http.rs`: `Http`,
`Request`, `Response`, `Secret`, `HttpError`, `is_slack_host` and the bound constants (`MAX_OPERATIONS`, `MAX_BODY`,
`MAX_REDIRECTS`). `api.rs`: `Api`, `HistoryQuery`, `Bounds`, `Upper`, `Continuation`,
`Page`, `PostKind`, `PostOutcome`, `Identity`, `ApiError` and the bound and deadline constants. `socket.rs`: `Socket`,
`Gap`, `GapCause`, `Status`, `SocketError` and the bound constants (`MAX_PENDING`, `MAX_PENDING_BYTES`, `MAX_MESSAGE`);
`http.rs` also holds `Http::websocket`, the one way to a websocket. `config.rs`: `Config`, `Credentials`, `Resolution`, `ConfigError`, `Folder`, `resolve`, `config_dir`. `fake_slack/` (test only): the scripted fake Slack
and its websocket peer (`ws.rs`).

## Calls in
`sot_protocol::chat::{ChatRecord, ChatAuthor, ChatFile, Origin, Ts}`, `serde_json::Value`; reqwest, rustls and
webpki-roots and tokio-tungstenite inside `http.rs` (`socket.rs` uses the library's `Message` and `Error` types; `api.rs` uses
`reqwest::Url` to build query strings). Dev only: `sot_platform::test_isolated` (`sot-platform` with `test-support`).

## Does not
Open a socket except through `Http::websocket`, call a file method outside `config.rs`, hold a credential at rest, read a token from the environment, file mail, or resolve a row (it maps folders to channels from supplied facts). Define the
record's representation (wire's). Offer another way to the network, a retry helper or a generic outbound HTTP type.

## Dependencies (the web stack)
Pinned as measured on the branch `stack/web-tls`: `reqwest =0.13.5` (no default features; `rustls-no-provider`, `json`,
`stream`), `rustls =0.23.46` (no default features; `ring`, `std`, `tls12`), `webpki-roots =1.0.11`, all in the workspace
table; `tokio` from the workspace; `tokio-tungstenite =0.30.0` (no default features; `connect`, `rustls-tls-webpki-roots`, the
pin measured on that branch) and `futures-util 0.3` for the socket; dev only: `tokio-rustls 0.26` (no default
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
`fake_slack/mod.rs` (test only) is an HTTPS server on loopback over real TLS (`tokio-rustls`), hand-written, scripted per
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
| 6 | verified server | `http_tests.rs`: `an_untrusted_certificate_fails_before_any_http`, `a_name_the_certificate_does_not_cover_fails_before_any_http` (each with a passing control; the second also the resolver's refusal) |
| 7, 8 | deadline, admission, body bound | `http_tests.rs`: `chat_operations_keep_bounds_after_caller_drop` |
| 9 | one send, one request | `http_tests.rs`: `one_send_is_one_request_even_when_the_server_closes_mid_response` |
| 10 | no secret in a formatted value | `http_tests.rs`: `chat_secrets_stay_inside_daemon_boundaries` |
| 11 | exact history pages | `api_tests.rs`: `history_pages_exact_query_without_silent_completion`, `a_bound_without_exactly_six_fraction_digits_is_refused_before_sending` |
| 12 | one post, origin kept | `api_tests.rs`: `lost_post_answer_is_unknown_and_never_resent`, `a_post_carries_its_author_and_every_unconfirmed_answer_is_unknown`, `a_post_read_back_through_history_keeps_its_origin` |
| 13 | retry header | `api_tests.rs`: `retry_header_paces_reads_and_preserves_mutation_draft` |
| 14 | deadlines | `api_tests.rs`: `every_operation_ends_at_its_deadline`, `name_lookups_together_end_at_the_names_deadline` |
| 15 | no raw Slack text | `api_tests.rs`: `slack_text_and_identity_failures_reach_no_caller_raw` |
| 16 | names | `api_tests.rs`: `names_fall_back_to_ids_without_unbounded_cache` |
| 17 | ack first (C11) | `socket_tests.rs`: `socket_ack_precedes_slow_api_and_dispatch` (the fake sends each envelope only after the previous one's acknowledgement while the dispatcher is stalled on a lookup that never answers), `a_write_that_cannot_finish_retires_the_socket_within_the_write_bound` (a peer that never reads fills the buffers; the socket is dropped, not held, with idle far past the test) |
| 18 | one socket (C12) | `socket_tests.rs`: `disconnect_refresh_and_silent_close_keep_one_socket` (warning, refresh, silent close, a peer that ignores close, endless refreshes; peak open 1; stop bounded and final), `a_peer_that_ignores_close_is_forced_down_within_the_bound`, `the_product_numbers_are_the_measured_ones` (all eight product timings), `a_silent_socket_is_replaced_after_the_idle_bound` (ping shorter than idle, as in the product; replaced no sooner than idle and within a stated bound), `a_link_that_answers_is_kept_past_the_idle_bound`, `a_chatty_peer_that_never_answers_keeps_its_link`, `a_peer_that_only_pings_keeps_its_link` (a peer ping alone resets the idle clock), `every_slack_ping_is_answered_with_its_payload_while_the_consumer_stalls_and_the_queue_is_full` (a pong frame with the payload, in both states), `a_bare_end_of_stream_is_replaced_within_the_pacing_and_latches_a_gap`, `reconnects_are_paced_after_a_refusal_and_after_a_hello`, `stop_during_the_reconnect_pause_does_not_wait_for_it` |
| 19 | gaps (C13) | `socket_tests.rs`: `a_gap_opened_by_an_outage_survives_a_reconciliation_made_during_it`, `a_failed_event_or_a_lost_socket_during_a_reconciliation_keeps_the_gap`, `a_reconciliation_begun_before_a_hello_is_refused_by_it`, `overflow_latches_gap_without_blocking_ack` (a failed event; 300 envelopes over the count with every one acknowledged; a loss during a reconciliation survives it; ten 900,000-byte events over the byte budget), `an_events_message_keeps_its_origin_through_the_socket` |
| 21 | configuration (M1, M2, C14's load half) | `config_tests.rs`: `mapping_refuses_aliases_duplicates_and_inert_anchor`, `resolve_explains_unmapped_invalid_and_disabled_delivery`, `a_second_workspace_duplicates_and_bad_values_are_refused_at_load`, `a_token_prints_no_bytes_and_a_load_error_quotes_no_secret_value`, `loading_the_configuration_opens_no_secret_file` (an observing reader), `the_files_are_named_in_platforms_config_folder` |
| 20 | websocket bounds and destinations | `socket_tests.rs`: `a_message_over_one_mib_is_refused_before_it_is_read_whole` (one byte over is not delivered, exactly the bound is), `a_foreign_websocket_url_fails_visibly_and_leaks_nothing` (also eight spellings refused by `Http::websocket`) |

## Files
- `Cargo.toml`: the manifest; the dependencies are listed at the head of this page.
- `src/lib.rs`: the modules and re-exports.
- `src/normalize.rs`: the normalization. `src/config.rs`: the configuration; `src/config_tests.rs`: its tests.
- `src/http.rs`: the HTTP core and `Http::websocket`. `src/api.rs`: the Slack calls; `src/api_tests.rs`: their tests.
- `src/socket.rs`: the Socket Mode listener; `src/socket_tests.rs`: its tests.
- `src/fake_slack/`: the test-only fake, its websocket peer and its certificates. `src/http_tests.rs`: the HTTP tests.

## Start here
`src/normalize.rs`, `normalize`; `src/http.rs`, `Http::send`; `src/api.rs`, `Api::history` and `Api::post`; `src/socket.rs`, `Worker::run` and `Worker::handle`.
