# rust/chat: chat (contract)

The project chat's policy. This stage holds two pieces: normalization (Slack's raw message JSON into the wire's one
record, `sot_protocol::chat::ChatRecord`), `http`, the one way out of the crate to the network, and `api`, the Slack
Web API calls the first stage needs (`auth.test`, history and replies, `chat.postMessage`, `users.info`). The crate
(`sot-chat`) depends on `sot-protocol`, `serde_json` and the web stack recorded below; nothing links it but the
workspace (`sotd` does not yet). It owns no runtime state beyond `Http`'s client and admission count and `Api`'s bot
identity, read-pacing instant and name cache, and has no socket, no file call, no token or configuration loading, no
delivery and no channel mapping.

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

The timestamp rule (exactly `<seconds>.<six digits>`) is wire's `Ts`; this crate builds only `Ts` values, so a record
never holds another form.

## Owns
`normalize.rs`: `normalize`, `Context`, `Normalized`, `Unrepresentable` and the bound constants. `http.rs`: `Http`,
`Request`, `Response`, `Secret`, `HttpError`, `is_slack_host` and the bound constants (`MAX_OPERATIONS`, `MAX_BODY`,
`MAX_REDIRECTS`). `api.rs`: `Api`, `HistoryQuery`, `Bounds`, `Upper`, `Continuation`,
`Page`, `PostKind`, `PostOutcome`, `Identity`, `ApiError` and the bound and deadline constants. `fake_slack/` (test
only): the scripted fake Slack.

## Calls in
`sot_protocol::chat::{ChatRecord, ChatAuthor, ChatFile, Origin, Ts}`, `serde_json::Value`; reqwest, rustls and
webpki-roots inside `http.rs` alone (`api.rs` uses `reqwest::Url` to build query strings).

## Does not
Open a socket, call a file method, hold a credential at rest or load one, read configuration, file mail, or map
channels to rows. Define the
record's representation (wire's). Offer another way to the network, a retry helper or a generic outbound HTTP type.

## Dependencies (the web stack)
Pinned as measured on the branch `stack/web-tls`: `reqwest =0.13.5` (no default features; `rustls-no-provider`, `json`,
`stream`), `rustls =0.23.46` (no default features; `ring`, `std`, `tls12`), `webpki-roots =1.0.11`, all in the workspace
table; `tokio` from the workspace; dev only: `tokio-rustls 0.26` (no default features; `ring`, `tls12`) and `base64 0.22`
for the fixture. The change adds 37 `[[package]]` entries to `Cargo.lock`. `sot-chat`'s own tree
(`cargo tree -p sot-chat --target all -e all`) holds no aws-lc, openssl-sys, cmake, pkg-config, vcpkg, bindgen or
native-tls (`wasm-bindgen` and `wit-bindgen`, on wasm targets, are other crates); of those crates the workspace lock
holds only pkg-config, through the frontend. The tree does hold reqwest's `rustls-platform-verifier` and its per-OS
readers (`rustls-native-certs`, `openssl-probe`, `schannel`, `security-framework`), compiled and never used, because
`http.rs` hands reqwest a finished rustls configuration. The one native build is `ring` through `cc` (a C compiler for the
target; the gate's `darwin-check` cannot build it from a Linux host with no Apple clang, so the line fails the day a
crate links `sot-chat`). `tokio-tungstenite` joins with the socket.

## The fake Slack
`fake_slack/mod.rs` (test only) is an HTTPS server on loopback over real TLS (`tokio-rustls`), hand-written, scripted per
test (`Reply`: a status, raw bytes, a hang, a stall after bytes, an endless body, a dropped connection), recording each
request (with its path, query parameters and JSON body), connection and handshake failure. It flushes after every
write and its listener has a 4 KiB send buffer, so output left unflushed in the TLS session hangs on Linux as it did on
Windows (`chat_operations_keep_bounds_after_caller_drop`, `/exact`, red until the flush). Its listener and connection tasks belong to the `FakeSlack`: `shutdown`
joins them within five seconds and a drop aborts them. Its fixed certificates (a CA valid to 2126 whose key was discarded,
a CA-signed server certificate, a self-signed one) and their generation are described in its header; `.gitleaks.toml`
allows its two `.key` files by exact path and no other file.

## Tests
Paths are under `rust/chat/src/`.

| # | guarantee | test |
|---|---|---|
| 1 | result for every shape | `normalize_tests.rs`: `normalize_members_bots_edits_deletes_and_file_stubs`, `hostile_json_gives_an_explicit_result` |
| 2 | own posts by supplied identity only | `normalize_tests.rs`: `normalize_members_bots_edits_deletes_and_file_stubs` (member prefix, member username, other bot), `with_no_identity_supplied_a_member_with_a_forged_prefix_is_a_member`, `a_prefix_in_an_own_posts_text_is_only_text`, `an_unreadable_own_post_keeps_its_origin_and_an_unreadable_reply_its_thread` |
| 3 | bounds | `normalize_tests.rs`: `text_is_cut_on_a_character_boundary_and_marked`, `names_filenames_and_mime_are_cut_and_marked`, `only_eight_files_are_summarized_and_the_rest_counted`, `an_id_over_64_bytes_or_not_ascii_is_not_carried`, `a_record_never_exceeds_32_kib_of_json`, `names_lose_control_characters`, `a_normalized_record_round_trips_through_the_wire` |
| 4 | attachment only | `normalize_tests.rs`: `an_attachment_only_message_keeps_its_files_and_has_no_text` |
| 5 | Slack destinations and authorization | `http_tests.rs`: `private_redirect_keeps_slack_auth_and_refuses_foreign_destinations`, `slack_hosts_are_slack_com_and_its_dot_boundary_subdomains`, `a_mutation_is_sent_once_and_never_followed_to_a_redirect`, `an_ambient_proxy_variable_is_ignored` (the test re-runs itself as a child with every proxy variable naming a dead address and expects a direct request), `the_product_client_builds_and_refuses_off_port_plain_and_ip_destinations` (no network) |
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

## Files
- `Cargo.toml`: the manifest; depends on wire and the web stack.
- `src/lib.rs`: the modules and re-exports.
- `src/normalize.rs`: the normalization.
- `src/http.rs`: the HTTP core. `src/api.rs`: the Slack calls; `src/api_tests.rs`: their tests.
- `src/fake_slack/`: the test-only fake and its certificates. `src/http_tests.rs`: the HTTP tests.

## Start here
`src/normalize.rs`, `normalize`; `src/http.rs`, `Http::send`; `src/api.rs`, `Api::history` and `Api::post`.
