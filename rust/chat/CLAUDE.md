# rust/chat: chat (contract)

The project chat's policy. This stage holds normalization (Slack's raw message JSON into the wire's one record,
`sot_protocol::chat::ChatRecord`), `http`, the one way out of the crate to the network, `api`, the Slack Web API calls
the first stage needs (`auth.test`, `bots.info` for the bot's app, and `identify`, the setup's `auth.test` before a workspace is configured, history and replies, `chat.postMessage`, a file's share (`files.getUploadURLExternal`, the upload, `files.completeUploadExternal`), the fetch of a picture of a channel (`files.info`, one GET of the file's URL), `users.info`, `conversations.info`, the picker's `conversations.list` and `conversations.join`), and `socket`, the Socket Mode
listener, and `config`, the two configuration files, and `service`, the running listener of one workspace, and `watch`, the model of the watched channels, and `mail`, one record as the body of one mail message, and `state`, a delivering channel's boundary file and filed set, and `traverse`, the recovery walk of one channel and its threads, and `delivery`, the filing queue, the schedule of passes and the commit of the boundary. The crate (`sot-chat`) depends on `sot-protocol`, `sot-platform` (the config folder), `toml`, `serde_json` and the web stack recorded below. `sot-backend` depends on it, and `sotd`'s `rust/backend/src/chat.rs` is the only code that names it (the caller paragraph is guarantee 21's). It owns no runtime state beyond `Http`'s client and admission count, `Api`'s
bot identity, workspace name, read-pacing instant, name cache, closed flag and the transfer permits it draws on (`Transfers`, `MAX_TRANSFERS`, shared by the services given one, guarantees 32 and 33), a `Socket`'s one task, queue and gap, a `Service`'s workspace lock, phase, the claim that makes `Service::watches` give one `Watches` (three states: free, held until its `Watches` is dropped, and ended by a stop or an end for good) and its end (the first code it ended with on its own, guarantee 22), and a `Watches`' channels (bounded, guarantee 23) and its set of delivering channel ids (bounded by the configuration alone, guarantee 23; it holds no record), and a `Delivery`'s filing queue (at most `MAX_QUEUED` records), the one filing in flight and each delivering channel's `State` (guarantee 27). `traverse::pass` holds nothing between calls. Its file calls are `config`'s two reads (guarantee 21), the one lock file `service` takes through platform (guarantee 22) and `state`'s one file a channel (guarantee 25); it reads no token from the environment.

## Guarantees
1. **Every message shape gives a result, never a panic or a silent drop.** `normalize(ctx, channel, raw)` returns
   `New` (a message), `Edited` (a changed message under its original identity, so a caller can tell it from new work),
   `Deleted` (the identity to remove), `Unsupported` or `Unrepresentable` (not an object, a bad workspace, channel or
   timestamp, a wrong-typed `subtype` or edit body): the caller treats `Unrepresentable` as a gap. `Unsupported` is neither a gap nor a message: the
   subtype is not on the list of displayed ones (none, `thread_broadcast`, `file_share`, `bot_message`, `me_message`;
   joins, tombstones and any subtype Slack adds later), and the caller skips it and logs the raw subtype. A message with a valid `ts` whose other fields cannot be read becomes
   a record with `omitted` set and no content, not a dropped message; it keeps its author id, its origin (an own post or any message with a `bot_id` key, even an unreadable one, is `Bot`, never
   `Member`) and its thread, each read on its own; an unreadable `thread_ts` falls back to `ts`. `edited` is set when the message, or a `message_changed`'s inner message, has an `edited` object (Slack's form, not yet seen live: look plan premise PC); the key with any other value, and an `omitted` record, are not edited. The flag is in the encoded record the 32 KiB bound of guarantee 3 measures (`normalize_tests::the_edited_flag_counts_inside_the_record_bound`: a record exactly at the bound without the flag is cut with it). Limit: the author's display name is the event's own
   profile name or empty, and `names` is empty; `Api::fill_names` and `Api::cached_names` (guarantee 16) fill them.
2. **Own posts are decided only by the supplied bot identity, and a forged marker never makes one.** Own-app is
   `Context.own_bot_id` or `own_user_id` matching the message; an identity not supplied matches nothing. Only an own
   post has its origin recovered, in order from `metadata`, then the `username` suffix ` (<kind> via Ship of Tools)`,
   giving `Drawer` (person) or `Row`, then, for an own `file_share` alone (a file share takes no custom author or
   metadata, guarantee 32), from the row's label its text starts with as `Api::post_file` sent it: the escaped `<name>
   (row via Ship of Tools)`, ended by the first marker (a person's share carries no label and reads as theirs, so a
   person's label on an own share is no label and reads `Bot`), alone or followed by `: `, its name one `post_file` accepts
   and escaped exactly so; the record's text is then what follows the label (the comment, or empty). An own post with
   none of these is `Bot` (display only), its text untouched, even when the text carries a marker. A member who types a
   marker, a forged `username` or `metadata` from anyone else, and another bot stay `Member` and `Bot`. Limits: a
   name that itself holds a marker followed by `: ` reads back cut at that marker (the origin is still the app's own
   kind of post); that Slack lists the share as a `file_share` from the app with the comment as its text is unmeasured
   until a person's first use, and if not, the share reads as `Bot` (never mail). A person's post made with their own
   user token is not an own post: it has their `user`, no `bot_id` and none of the app's marks, so it reads `Member` by
   this same rule, and nothing new reads it. Unmeasured, and what the goal rests on: that Slack's message for a post
   made with a user token has no `bot_id` and no `subtype`. Check, at the first real post: a message typed in Slack's
   own app in a channel where the responder answers is answered, then a post from the drawer into the same place; the
   drawer shows it under the person's name with no `APP` badge, and the responder answers it. If Slack gives the post
   a `bot_id`, the responder skips it and `normalize` reads it as `Bot`, and nothing in this tree fixes that
   (`CHAT-PERSON-POST-NOT-ANSWERED`). A `Drawer` record is a post an older daemon made as the app, before a person's
   post went out under their own token; nothing makes one now (`CHAT-DRAWER-ORIGIN-LEGACY`).
3. **A record is bounded, and a cut is marked.** 32 KiB of encoded JSON, 8 KiB of text, eight file summaries (the rest
   counted in `files_omitted`, as is any file entry with no valid id), 64 ASCII-byte ids, 160 / 256 / 128 bytes for an
   author name / filename / MIME. A cut falls on a character boundary and sets `truncated` (which covers text, names,
   filename and MIME); the record's size is held by halving the text until it fits, and once the text is gone the other bounds (ids, names, eight files, three timestamps) keep it near 8 KiB, far under the cap, so nothing else is cut. The halving aims at `MAX_RECORD_BYTES` less `FILL_RESERVE`, the most a fill adds to an encoded record (sixteen `names` entries of a 64-byte id and a 160-byte name escaped to twice its size, with their commas and key, and an author name of 160 bytes escaped the same way), so a filled record is within the 32 KiB too (`normalize_tests::a_filled_record_stays_within_the_record_bound`: a record exactly at the reserved bound is exactly 32 KiB with the worst fill). A record's text is Slack's escaped markup (`&amp;`, `&lt;`, `&gt;` stay as Slack sent them), and the byte cut can split a token. Names, filenames and MIME lose control characters and are trimmed of surrounding whitespace (`clean`), and nothing else is stripped: direction and other format characters stay, for the later display and mail rules. A
   file stub (`file_access: check_file_info`) keeps its id only. A private URL is never copied; a thumbnail is a flag.
   Limit: `truncated` does not say which field was cut.
4. **An attachment-only message is a record with empty text and its files.** Nothing needs a caption.

5. **A request goes only to an HTTPS Slack destination, and the authorization (the bot's token, or a person's user token) rides only on those hops.** The first
   URL and every redirect are checked before any request: scheme `https`, no userinfo, port 443, a domain host that is
   `slack.com` or a dot-boundary subdomain with every label non-empty. `evilslack.com`, `slack.com.evil.example`, an IP
   host, `http`, userinfo (including `host:port@evil`), another port and a scheme-relative redirect to a foreign host are
   `Destination`; an unparseable `Location` is `Redirect`. The authorization header is built anew for each hop and is
   marked sensitive. Only a GET follows redirects, at most three; a POST that is redirected, or a loop, is `Redirect`.
   Automatic redirects and ambient proxies are off. Limit: the one predicate is destination and authorization alike, so
   a request without the authorization goes only to Slack hosts too: a file's upload (guarantee 32) posts its bytes,
   unauthenticated, to the URL Slack answered, and one off Slack is `Destination` before a connection; a fetch
   (guarantee 33) sends the authorization to the file's URL, which is checked the same way, and a redirect of it off
   Slack's hosts is `Destination` before any request there; a non-Slack
   unauthenticated destination (a CDN, an upload host elsewhere) is a later, reviewed addition.
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
   `test-support` (or sot-protocol's, which builds only `ChatRecord::fixture`) is in it, whether set on a normal dependency of any crate (also under a target table), in `default`, or by
   a member only `--workspace` builds; it runs in every gate run with each of those faults put in (`--prove`), and needs the
   release targets' packages in the depot (`cargo fetch --locked` once). Third: `--prove` also runs a release-profile check of `sot-chat` with the feature, which must fail with that compile error; deleting the `compile_error!` turns `--prove` red. Source builds by users (`launch-sot.sh`, `sot-freshness.ps1`) are `--release`, so the compile error covers them; a release-profile test build is refused by design. Limits, resting on review: a debug-profile build with the feature turned on by a command-line flag, a release profile that turns debug assertions on, a
   re-export of `for_test` or the fake outside its cfg, and code written to include the fixture's files from outside the
   gated module.
7. **An operation has one owner-held deadline and bounded admission.** `Request` carries its deadline; `send` runs the
   operation as a task that holds the deadline and the permit, so connection, headers, redirects and body share one
   bound and a caller that stops waiting neither frees the permit nor cancels a mutation already sent. Sixteen
   operations are admitted; the seventeenth is `Busy` at once. Limit: the permit returns only when the task ends
   (the deadline at the latest); a name lookup that outlives it keeps its blocking thread until the resolver returns.
8. **A response body is bounded while it streams.** At most 1 MiB, or the request's own bound where it sets one (a
   scan's is `MAX_SCAN_BODY`, 16 MiB; a page over it is `TooLarge`, and what the walk does then is the traversal's),
   counted as chunks arrive (a fetch's is `MAX_THUMB` or `MAX_CHAT_FILE`, guarantee 33), whatever `Content-Length` claims (a gigabyte claimed over a two MiB body, an endless
   chunkless body and one byte over the bound are `TooLarge`; a body of exactly the bound passes; a short claim ends the
   body at the claim).
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
   **No thread read follows a cursor under a lower bound alone.** `Api::history` refuses a continuation for a replies
   query with `after` and no upper bound (`bad_request`), unsent: in that form Slack's cursor was measured live dropping
   one reply at every page boundary, never returned, with a last page that says it is complete (SlackClaw's facts note,
   "Page order with `oldest` alone, measured"). That form's first page, which loses nothing, and the drawer's reads (no
   bound, or an upper one) are sent as before.
   **`Api::scan` is delivery's one read.** `ScanQuery` needs `after` below `before` (an `until`, a missing bound or an
   empty or inverted window is `bad_request`) and a limit of 1 to 999 (`MAX_SCAN`); the read sends `oldest`, `latest`
   and `inclusive=false`, never a cursor, under its own response bound (guarantee 8). A `ScanPage` gives the records
   inside the bounds (a replies page's parent only when it is inside them; it is always in `threads`), `oldest` and
   `newest` as the raw extremes of the page's `ts` (a replies page's parent apart, whatever order Slack lists them),
   `threads` as every message's `(ts, latest_reply)` as Slack sent it (a deleted parent's, and one above the read's
   `latest`, which Slack was measured to send, included), `has_more`, and `limited` as Slack's flag, reported and never an
   error. A replies page holds its limit plus the parent (1,000 at 999); a page with `has_more` and nothing but the
   parent, or nothing at all, is `Malformed` (no walk can start from it), and a replies page of the parent alone with
   `has_more: false` is a page, the measured end of a walk. A message with no valid `ts`, or a `latest_reply` that is
   not one, fails the page. The walk that uses it, a fresh read per page with `latest` lowered to the page's oldest raw
   `ts`, was measured to return every message exactly once, also while posts arrive (SlackClaw's facts note, "The walk
   back with no cursor, measured" and "Posts arriving during reads and the walk (P-ORDER), measured"); its first read
   must carry `latest`, since `oldest` alone returned only the oldest page and then stopped.
12. **A post is submitted once, and a person's goes out as that person.** `Api::post` of kind row sends one
   `chat.postMessage` with the bot token, the custom `username` (`<name> (row via Ship of Tools)`) and `sot_chat_post`
   metadata (kind, name), so `normalize` recovers the origin the same way over history; there is no text prefix. Of kind
   person it sends one `chat.postMessage` under the person's own user token (`xoxp-`, `Api::with_person`) carrying
   `channel`, the escaped `text` and `thread_ts` when it replies, and nothing else: no `username`, no `metadata`, no icon
   and no label, so Slack and any other responder see an ordinary message from the person. The name is neither checked
   nor sent for a person's post (a row's is checked: `Err` unsent). With no user token held it is
   `Err(ApiError::NoPersonToken)` (code `no_person_token`), unsent, and the bot's token is never used in its place. When
   Slack refuses a person's post with a code of `TOKEN_REFUSALS` or `missing_scope`, the API forgets the token before it
   answers (the next person's post is refused unsent, `NoPersonToken`; the answer is still `Refused(code)`; the file keeps the token, so a
   restart reads it again); any other refusal, `not_in_channel` included, keeps it, and a row's refusal never touches it (a row's refused with a code
   of `TOKEN_REFUSALS` ends the service instead, guarantee 22).
   At a service's start the held user token is checked once (`Api::check_person`, one `auth.test` under it): a
   `team_id` other than the service's workspace, or a code of `TOKEN_REFUSALS`, drops it from the service (a person's
   post is then `NoPersonToken`, unsent) with a fixed code, `Service::person_dropped` (`user_token_other_workspace`,
   `user_token_refused`), which the daemon logs; Slack's own state or the transport keeps it
   (`service_tests::a_user_token_of_another_workspace_is_dropped_at_start`).
   Limits, noted and not fixed: a 401 status is `Refused("http_401")` and keeps the token (Slack answers `invalid_auth`
   as a 200 with `ok: false`, as far as is known); a bad `user_token` in the file turns the whole chat off, as a bad
   token of any key does; with the app's mark gone, a post forged by a local process of the account that declares itself
   a window is indistinguishable from the person's own (the same boundary as reading the secrets file). Also not fixed, from
   the review of the whole change: a user token of another person in the same workspace passes the connect (`auth.test`
   cannot say whose it is) and the hub then posts as them; Slack may answer a person not in a private channel
   `channel_not_found` and not `not_in_channel` (then `failed`, "the chat request failed", and not `not_a_member`; check at
   the first real post); a user token of another workspace is `user_token_refused`, whose words say Slack did not accept the
   token; after a connect on a hub whose row was already mapped, Enter on the connected screen opens the picker; a person
   with an old hub must paste all three tokens again to add one; a dev daemon of main before this change ignores
   `user_token`, saves two tokens and answers `{workspace}`, which the window then calls three tokens; and a connect's
   `save_tokens` reads `chat.toml` through `Chat::load`, which logs the no-user-token warning once more for a three-key hub. `Refused` is Slack's definite rejection (a non-5xx status, or `ok: false` with a
   code that is not transient) or a request that never left (`busy`, `invalid`, `destination`); `Limited` is a 429 or
   `ratelimited`; every other case without a valid acknowledgement (transport error, timeout, 5xx, unparsable or `ok`-less
   body, transient code, `ok` with no `ts`) is `Unknown`. Nothing resends, and a caller that stops waiting does not
   cancel or repeat the request. Input a post cannot carry (id, name, text over 8 KiB, thread `ts`) is `Err` unsent. Every message text this crate sends is plain text made Slack's by one private function (`escape_text` after `post_text_fits`, wire's), the only way a request body gets a `text`; the 8 KiB bound is on the escaped size, and no post can mention a person or ping a channel until a structured field adds one. The daemon's hello says so (`HelloRes.posts_plain_text`).
13. **A retry header paces reads, never repeats a mutation.** The `Retry-After` header, in any case, in whole seconds
   (capped at one hour), makes every read refuse as `Limited` without a request until it passes; a missing or invalid
   header is an assumed 30 s (`assumed: true`). A post that is limited is returned with its wait and is not gated by the
   reads' wait.
14. **Every operation has a stated deadline.** Reads 15 s, a post, a join and a file's share 30 s (none is resent, so an
   early give-up only makes `Unknown`), a file's upload and a fetch's original 60 s (`TRANSFER_DEADLINE`), a fetch's thumbnail 15 s, all of a page's name lookups together 10 s; each through `Http`'s owner-held deadline.
15. **Slack's text reaches no caller.** `ApiError` is a fixed code; Slack's `error` is carried only when it is
   `[a-z0-9_.]{1,64}`, else `unknown_error`; bodies and URLs are dropped; `auth.test` for another workspace is
   `WrongWorkspace`; history without the bot identity is `NoIdentity`, unsent.
16. **A name lookup that fails shows the id and is not remembered; the cache is bounded.** `author_name` and
   `fill_names` take `display_name`, then `real_name`, then `name`; only a success is cached, at most 1,024 entries,
   the oldest leaving first. `fill_names` looks up, in one 10 s deadline and through the same cache, each member author
   with no name, then the distinct ids of the `<@U…>` and `<@W…>` tokens in each record's text, then those of its `<#C…>` and `<#G…>` tokens that carry no label (a labeled one is not looked up and shows its label, unless the record holds that id's name from a label-less token of it; together at most `MAX_MENTIONS`,
   16 per record, the people first and then the channels, each in the text's order, so a full text gives up a channel before a person: `people_come_before_channels_under_the_cap_of_names`; a token a byte cut left open has none; the lookups run authors, people, channels, so a deadline that runs out costs a channel's name before a person's and a person's before an author's), and sets each record's `names` (and an author's name) from what its own lookups found, held in the fill and not read back from the cache, which a fill of more than 1,024 lookups would push its first names out of (`api_tests::fill_names_keeps_what_it_found_when_the_cache_cannot`; `fill_names_looks_up_authors_before_mentions` pins the order), in the text's order; a
   failed lookup adds no entry, and a lookup past the deadline is none, so the window shows the token's label or the id.
   `cached_names` does the same from the cache alone, with no request: the daemon calls it on every put of a
   `chat.changed`, so a live record carries any name a history read has looked up, its author's included (empty
   when the cache lacks it, where `fill_names` would show the id). The ids are looked up with `users.info`, a channel's with `conversations.info` (its `channel.name`, cleaned; `channel_name` is the one rule of which channels may be named, reading `conversations.info` through `Api::channel` (guarantee 29): a mapped channel, which the start's lookup asks for through `channel_names`, is named public or private, since the hub's owner mapped it, the bot reads it and the drawer shows its messages, and any other, a mention's, only when its `is_private` is `false`, so a private one's name, or one from a reply that does not say, is not added and the channel shows its label or its id as Slack shows a channel a reader may not be in; a mapped private channel's name is then in the cache, so a mention of it may show the name: `a_mapped_private_channel_is_named_and_an_unmapped_one_is_not`), and cached by id (a person's id and a channel's never collide; `a_channel_mention_without_a_label_is_looked_up_once`). Unmeasured: whether `conversations.info` answers for a channel the bot reads, and which scopes it needs, are Slack's documented reply and errors, not seen live (the look plan's premise PD); a failed lookup adds no entry, so the window shows the label or the id. A fill ends its channel lookups at the first scope or authorization error (`missing_scope`, `not_authed`, `invalid_auth`, `token_revoked`, `account_inactive`, `not_allowed_token_type`) or rate-limited answer, so a missing scope costs one request a fill and channel names never earn a wait (`a_fill_stops_its_channel_lookups_at_a_scope_error_or_a_rate_limit`; the rate-limited case holds without the stop, because a limit already refuses every read until it passes). Limit: a failure is not remembered, so a channel Slack does not name (not found, private, an empty name) is asked again by every fill, up to 16 per record within the 10 s; a success is never refreshed within a process; a lookup that outlives the 10 s
   bound keeps its `Http` permit to that operation's own 15 s deadline.

17. **Each envelope of the bot's own app is acknowledged, by its id, before any event work; one of another app never
   is.** The reader parses the frame and then writes `{"envelope_id": <id>}` as the first thing it does with a frame that
   carries one (an id over 128 bytes is not echoed and is a gap), whatever the frame's type, before normalization or the
   queue; the queue push never waits, so a consumer stalled on a name or file lookup, or a full queue, cannot delay an
   acknowledgement. The one exception is checked first: a frame of any type (a `hello`, an `events_api`,
   `slash_commands` or `interactive` envelope, any other) whose `connection_info.app_id` or `payload.api_app_id` is an id
   other than the bot's app (`Identity.app_id`, guarantee 22) is not
   acknowledged, the socket is retired, no next socket opens, and the listener ends for good as `wrong_app` (its
   service with it). Unacknowledged, the envelope can go to that app's own consumer (Slack's documented redelivery,
   unmeasured here); the hub never takes another app's events. It fails open: a frame without the field, or one whose
   value is no id, and a bot whose app is unknown are no mismatch, as before. The team is not compared: an event of a
   shared channel may name another organization's team, unmeasured. Limit: a write that does not finish in 5 s retires
   the socket.
18. **One socket at a time, and a replacement waits for retirement.** One task owns the listener and its one socket. It
   opens the next socket only after the last is retired: `warning`, `refresh_requested`, any `disconnect`, a peer close, a
   silent close (no close frame), 45 s without a received frame (the clock restarts on a frame received, never on our own ping, every 30 s) and an
   acknowledgement that cannot be written all end the socket the same way. Retirement is a close the peer must answer within
   2 s (Slack's observed behavior: a peer that never answers blocks a read forever), then the connection is dropped. Reconnect
   is paced: 1 s, doubling to 30 s while attempts fail, reset by a `hello`, so a refusing Slack is not hammered and a healthy
   refresh is back in a second. Two ends are for good, with no next socket: a frame of another app (`wrong_app`,
   guarantee 17) and an `apps.connections.open` refused `Rejected` (`app_token_refused`, guarantee 20); each raises the
   service's end, the task ends and its lock goes with it (guarantee 22). `stop` sets the signal, the task retires the socket and ends, and `stop` joins it within 5 s
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
   before a socket was live are never covered by a reconciliation that began before it. `Unsupported` subtypes and non-message events are neither. A frame of another app is not acknowledged and latches no
   gap: the listener ends, which its caller reads (`Service::ended`). A gap stays until
   `Socket::reconciled(gap)` is given the value read before a complete reconciliation began; it clears only when nothing
   was lost since (every latch is a new sequence number, process-wide), so a loss during a reconciliation survives it, whether it is an overflow,
   a failed event or a lost socket.
20. **A websocket obeys the request rules and its own bounds.** The URL from `apps.connections.open` passes the guarantee-5
   predicate with scheme `wss` (userinfo, another port, an IP, a foreign or lookalike host, `ws` and `https` are
   `destination`, before any dial), connects directly (no proxy) and runs through `Http`'s one rustls configuration. A
   message is at most 1 MiB, by the library's frame and message limits (a larger one is `Capacity`, a gap and a new socket,
   never read whole). The connection and handshake share a 15 s deadline. The app token rides only the
   `apps.connections.open` request, which is a request under guarantees 5 to 10; the private URL and the ticket reach no
   error, status or format value (`SocketError` is a fixed code: `Http(code)`, `Rejected`, `Unavailable`, `Malformed`, `BadRequest`;
   Slack's text is dropped). A failed open is `Status.error`; one whose answer's body carries Slack's own code naming the
   token (`ok: false` with a code of `api::TOKEN_REFUSALS` or `missing_scope`, at 200, 401 or 403) ends the listener for
   good as `app_token_refused` (premise PS11), and every other is paced and retried: a bare 401 or 403 with no such code
   (`Rejected` in the status, which a proxy or an edge can answer), `Unavailable` (Slack's own state) and the
   transport's failures. The setup's check (guarantee 28) still reads a bare 401 or 403 as `rejected`. **PS11, Slack's
   token codes are definitive** (unmeasured): Slack answers those codes only for a token that is bad, never for a passing
   state; if it is false, one stray answer ends the chat until the next connect or daemon restart. Limit: C14's lock and second service
   are guarantee 22; its load half is guarantee 21.

21. **The configuration is read one way, refused with a stated reason, and shows no token.** `chat.toml` (the file's
   workspace id, its default, `Config::workspace`; `channels`, `[[mapping]]` entries of `folder` and `channel`, an optional `mail_after` that is a `Ts`) and
   the secrets, one file per workspace, each `workspace`, `bot_token`, `app_token`, and an optional `user_token`, are at the top of platform's config folder (`config_dir`):
   `chat-secrets.toml` holds `chat.toml`'s workspace, in the one form an older daemon reads, and any other workspace W is
   `chat-secrets-<W>.toml`, in the same form (a subfolder is never used, since a row's file post refuses only the folder's
   top entries); only the named keys are accepted. **The secrets' load (`Secrets::read`, C2).** One call lists the config
   folder once and opens, each through the one `Reader`: `chat-secrets.toml`, which must hold `chat.toml`'s workspace
   (`SecondWorkspace`); and, in name order (the listing's own order is none), each entry named `chat-secrets-<id>.toml`
   whose `<id>` is a Slack id (an editor's `chat-secrets.toml~`, a `.bak`, a lower-case id, a name past the id grammar, a
   name that is not UTF-8, which cannot match the grammar, and a file in a subfolder are never opened), which must
   hold its `<id>` (`NameNotWorkspace`, which is also the code of a named file of `chat.toml`'s own id, never opened). The
   load opens at most `MAX_WORKSPACES` (8, wire's) files, `chat-secrets.toml` counted and every refused file counted; a
   named file past them is `TooManyWorkspaces`, unopened. A listed name that reads as absent (a dangling link) is
   `Unreadable`, never the chat's `Missing`. Each file is admitted or refused on its own, so one never turns another workspace off:
   `Secrets` answers the admitted workspaces with their file and `Credentials` in workspace order, each workspace whose own file was refused
   (`refusals(default)`: `chat-secrets.toml`'s under `chat.toml`'s workspace, not when it is absent, each named file's under
   its name's id), each named file refused or not opened by its name and code, and the named files listed (`named(default)`,
   computed from what was admitted and refused, the default's id left out); a listing that fails lists no named file. `Credentials::parse(text)` answers a file's workspace and tokens; which workspace a file may
   hold is the load's rule. `Config::mappings()` yields each mapping's folder, workspace and channel (a mapping's `workspace` key names
   any id, absent meaning the file's), and each request reaches only the service of its mapping's workspace (guarantee 21). Each file is read by `read_file` alone, which reads through platform's `read_regular`: a regular file (checked on the open handle; on Unix the open is non-blocking, so a planted FIFO returns at once, and a device or
   directory is refused; Windows applies no such flag and has no such test) of at most 64 KiB (`MAX_FILE`), text,
   read to at most one byte over the cap. The cap's job is to end a stall and bound the work a planted file causes (the
   channel check is quadratic and each mapping asks the filesystem), not to be large. A list or a table for `chat.toml`'s
   `workspace`, a `chat-secrets.toml` naming another workspace than it (`SecondWorkspace`) and a repeated channel are
   refused at load; a mapping may name its own `workspace` (any Slack id; without the key it is in the file's, and
   `Config::mappings` and `resolve` answer the mapping's), and a channel id is mapped once in the file whatever its
   workspace (`ChannelRepeated`; `config_tests::a_mapping_names_its_own_workspace_and_without_the_key_is_the_files`,
   `a_channel_id_is_mapped_once_whatever_its_workspace`). The bot token starts `xoxb-`, the app
   token `xapp-` and the optional user token `xoxp-` (`Credentials.user`, `None` when the key is absent, so a three-key file
   written before it still loads: rows post and mail runs, and a person's post waits for a connect; `config::user_token` is
   the pasted grammar), then letters, digits and hyphens, 512 bytes at most (a character platform's masker ends a token at, such as a quote or a parenthesis, is refused, so no tail of an admitted token prints in clear): a swap or equal tokens are refused, and every admitted token
   is a form platform's log masker knows. Mappings are admitted against the disk through a supplied canonicalizer (`Canon`: a folder's canonical
   spelling, or `None` for a folder that does not exist or is not absolute; chat resolves no row, and a mapping needs none at
   the start): a channel not listed, two entries for one canonical folder (an alias, or the same pair twice) and one
   channel for two folders are refused (M1). A mapping whose folder does not canonicalize is ignored, never refused:
   `Config::ignored()` names its ordinal (from 1), the daemon logs one warning per ignored mapping with the ordinal alone
   (no path), one warning per admitted secrets file that holds no user token (the file's name and fixed words, no data; `chat_tests.rs`:
   `a_load_whose_secrets_hold_no_user_token_warns_once_in_fixed_words`), one per secrets file refused or not opened (its
   name and its code) and one per admitted file of another workspace than the file's, which is not served
   (`a_hub_with_secrets_for_two_workspaces_serves_the_files_default_and_logs_the_other`); the ignored mapping's channel and written folder still count in the repeat checks and toward `MAX_DELIVERING`, so a file
   admitted today is not refused when a missing folder returns. The inert anchor is no load rule: it is refused per
   action, by `admit` and `resolve` (`Unmapped`). A `mail_after` on the hub or on a mapping that is not a `Ts` is `BadMailAfter`, which carries the mapping's
   ordinal for a mapping's. A hub `mail_after` makes the file delivering, and then more than 32 mappings (`MAX_DELIVERING`,
   the watch model's `MAX_CHANNELS`) is `TooManyDelivering` and a mapping without its own `mail_after` is
   `MappingWithoutFloor` with its ordinal. `Config::floor(channel)` is the later of the mapping's value and the hub's,
   and `None` while the hub's is unset, however many mappings carry a value; `Resolution::Mapped` carries that floor.
   The limit: the load checks that a floor is present, not that it is recent, so a mapping copied with another's
   `mail_after` line passes and its channel delivers from that time or the hub's, whichever is later. `resolve(config, folder)` matches the canonical folder exactly and answers `Mapped` (delivering only
   with a `mail_after`), `Unmapped` or `Invalid(code)` (missing or malformed), each with a fixed `explain()`, and the
   daemon's own `NotHub` (M2: the daemon decided it is not the hub before any load, because another daemon on a shared
   home has other rows and would read every mapping as unknown). A refusal is a `ConfigError` code: a
   mapping refusal carries the mapping's ordinal and a parse failure its line, and no value from any file is
   quoted. A key the file does not take is `UnknownKey`, which names the key only when a table of these files accepts it (the
   lists `CONFIG_KEYS`, `MAPPING_KEYS` and `SECRETS_KEYS` that `only` checks are the only source), so a key chat.toml
   takes found in the secrets file is named, and any other key, a token-shaped one included, is not; the library's parse errors, which
   quote the source, are never formatted. The code does not know its file: the daemon's `Status` display starts a
   refusal with the file it came from (`chat.toml: ...`, `chat-secrets.toml: ...`; a named file's refusal is its own
   line, `file=` its name), the one text the log line carries
   (`chat.status` and `chat.resolve` carry no words). Every secrets file has one form, the top-level keys, never a
   `[[workspace]]` table (`a_secrets_file_has_one_form`); the
   reference's two samples are parsed by `the_reference_samples_are_accepted_by_the_parsers`. Tokens are `Secret`s
   (guarantee 10's mask). `Config::load` opens `chat.toml` only; `Secrets::read` is the one call in the crate that lists the config folder or opens a secrets file (the daemon's `is_configuration` lists it too, for a row's file post). **The writes.** The daemon's setup is
   their one caller; each text is one the load admits. `fresh(workspace)` is a `chat.toml` of the workspace and
   `channels = []`. `secrets(workspace, bot, app)` checks each token against the load's grammar before any text is
   made (`BadToken`), parses the text back with `Credentials::parse`, and returns it as a `Secret`, so it formats as a
   mask. `pair(text, folder, channel, floor, canon)` edits the parsed table of the current text: a text the load refuses
   is refused as it stands (`PairError::Config(code)`), never rewritten; a channel that is not a Slack id is `BadId`;
   the folder's own mapping (matched by its canonical spelling) is dropped; for a channel, a mapping of it whose folder
   does not canonicalize is dropped (taken over), one to another existing folder is `ChannelTaken`, and `{folder,
   channel}` is appended, with `mail_after = floor` only when the file is delivering (no floor there is the load's
   `MappingWithoutFloor`), its channel listed; a channel whose mapping the edit dropped and that no mapping holds after
   it leaves `channels`; ignored mappings and every other key stay, comments and key order do not; a pairing that changes
   nothing is `None`; the result must pass `Config::parse` and fit `MAX_FILE`, or the edit is refused with that code.
   `Secrets::file_of(workspace, default)` is the one file a workspace's tokens are written to (C11): `chat-secrets.toml`
   for `default`, `chat.toml`'s workspace, `chat-secrets-<workspace>.toml` for any other, `BadId` for a workspace that is not a Slack id;
   a `SecretsFile` is one of the two shapes, so each workspace has exactly one file a connect can write.
   `write_config(dir, text, writers)` and `write_secrets(dir, file, secret, writers)` make a missing `dir` and write the file
   whole through an injected `Writers` pair, as `Reader` is the one read: `chat.toml` through `config`, the secrets
   through `secrets` alone; `Writers::platform()` is platform's `write_durable` and `write_durable_private` (owner-only
   from the temporary file's creation). They are the crate's only file writes outside `state.rs`. `Secret::expose`
   (crate-private) hands a value whole to the secrets write, its one product caller.
   **The caller (`rust/backend/src/chat.rs`, `Chat::load` and `Chat::begin`, C1).** One load, `Chat::load`, the boot's
   and each reload's, in order, and only as the hub: this computer has a name
   (`SOT_SELF_HOST` or its host name) and topology's current `sot_topology::load_in` either finds no `hosts.toml`, so a
   lone computer is its own hub, or parses and names this computer as its hub (messaging's `comm_topology_hub` with the
   name required); an invalid topology, a computer with no name and another hub are all `NotHub`
   (`a_computer_with_no_hosts_toml_runs_its_own_chat_and_a_broken_one_does_not`; `TopologyStore::refresh` keeps an
   earlier parse and is not used). The limit: on a shared home with no `hosts.toml` every daemon counts as the hub; the
   comm folder's lock (guarantee 22) still admits one listener and the others wait `held`; then `Config::load` against the
   daemon's rows through one canonicalizer (an absolute folder, resolved through symlinks, `.`, `..` and a trailing
   slash, de-verbatimed and in the system's letter case on Windows; a relative spelling or a folder that does not exist
   is unknown); then `Secrets::read`, each admitted workspace a unit of its own (C5: today's phase machine and launch, per workspace, so one workspace's failed, held, ended or stopped listener stops no other: `every_connected_workspace_starts_its_own_listener`, `a_workspace_whose_start_fails_leaves_the_others_running`, `a_held_workspace_waits_alone`); a workspace with no admitted file is `Secrets(code)` of its own file's refusal (`Secrets::refusals`; `chat-secrets.toml`'s for the default), `Secrets(Missing)` when it has none, never another's (`a_named_files_refusal_is_not_the_defaults`, `a_workspace_with_no_file_is_not_connected_and_its_refused_file_is_its_own`). `Chat::status_of(W)` is the file-level status first (`NotHub`, `Refused`, `Failed("lock")`), else W's unit's, else that; `Chat::status()` is the default workspace's, so a one-workspace hub reads as it did; then the lock folder (messaging's `chat_dir`: the comm folder made whole, then its `chat/` private; never made by chat itself). Then `begin`'s registration with
   lifecycle's `retire::participants()`, made by a hub whatever its load gave (an off chat too, so a start a reload makes
   later retires with the daemon; `a_hub_with_missing_or_refused_files_is_off_and_writes_nothing`,
   `a_lock_folder_that_cannot_be_made_leaves_the_chat_off_and_registered`), made with the units lock already held and kept until every unit's start task is in
   place (lock order: the units lock, then a unit's phase lock, never the reverse; a start's completion takes its phase
   lock alone; `closed` is the chat's, set and read under the units lock), so a retirement that reaches `request_stop`
   after the registration finds every start and aborts it (pinned by
   `a_retirement_reaching_request_stop_between_register_and_starting_finds_the_start` and, with two workspaces,
   `a_retirement_between_register_and_starting_finds_every_workspaces_start`, through a test-only hook in
   lifecycle's `Registry`, which pins the lock order; the refusal of a registration once retirement began, and then no
   start, is pinned by `retirement_that_began_first_refuses_the_start`); then the start on a task, so boot never waits on Slack. A non-hub opens no chat file and
   registers nothing; a hub with no `chat.toml` is off (`Refused(Missing)`) and writes nothing; a malformed one opens no
   secret file. Every open at the boot goes through the one `Reader` passed to `begin` (a reload's through
   `config::read_file`), and `Chat::load` is the only caller of `Config::load` and `Secrets::read`: the status, its
   words, `resolve`, a stop and a wait read memory. **The reload (`Chat::reload`; `chat.pair` and `chat.connect` call it, guarantees 30 and 31).** One at a time: a reload takes the held setup lock (`Chat::setup` gives the guard, `Setup`), so a holder
   edits the files and reloads under one lock with nothing to deadlock on
   (`a_holder_of_the_setup_lock_edits_the_files_and_reloads`). It stops what runs as a retirement does (a start
   aborted, or the delivery ended and the service stopped), every workspace at once until reloads are per workspace
   (docs/defects.md: CHAT-RELOAD-RESTARTS-EVERY-WORKSPACE), waits for each unit `Off` and for each old owner's end (the
   new service's `ensure_owner` would otherwise find the old owner running and start none:
   `chat_watch_tests::a_reload_starts_its_service_only_after_the_old_owner_ended`, through the hold point
   `chat.owner.end` between the old service's stop and its owner's end), loads both files again through `Chat::load` on
   the blocking pool (it canonicalizes on the shared home; the boot's load runs inline, before the daemon serves), and
   then, under the phase lock and only while the chat is not closed, swaps the configuration (so a service and its
   owner see one configuration for their life) and launches the start as the boot does; then it starts the owner
   (`start_owner`, with its name lookup) and answers the status the start ended in. It waits at most `SETUP_WAIT`
   (30 s) for the stop and again for the start, and logs a warning at each give-up, fixed words and no data: a stop
   that outlasts it starts nothing and answers `Stopping`
   (`a_reload_whose_stop_outlasts_setup_wait_starts_nothing_and_says_so`); a start that outlasts it goes on and the
   reload answers `Starting` (`a_reload_whose_start_outlasts_setup_wait_answers_starting_and_says_so`). Nothing retries
   a reload that gave up: the chat stays as the stop or the start leaves it until the next reload or a restart
   (`a_reload_applies_new_tokens_and_a_new_mapping_with_no_daemon_restart`, against the fake Slack: the second token's
   `auth.test`, the first service stopped and its lock taken at the second start's first try;
   `a_hub_booted_without_chat_toml_starts_at_its_first_reload_and_retires_with_the_daemon`;
   `two_reloads_at_once_run_one_after_the_other`). Each launch is numbered, and its start task completes only against
   its own `Starting`, so a late completion of an earlier launch never overwrites a later one (no test: the window is
   a log call wide). Nothing starts after retirement began: `request_stop` marks the chat closed under the phase lock,
   and the launch and the swap check the mark under the same lock, so a start a reload launched first is ended by the
   stop and none begins, and no configuration is swapped, after (`a_reload_after_retirement_began_starts_nothing`,
   `a_retirement_during_a_reload_stops_what_it_started`,
   `a_retirement_while_a_reload_waits_for_the_stop_starts_and_swaps_nothing`). A chat that never registered (not the
   hub at its boot, or a registration refused) is closed from the start, so a reload on it stops, reads and swaps
   nothing and starts nothing, even once the topology names it the hub (its configuration stays `None` and no load makes
   the lock folder); a computer that becomes the hub starts its chat at its daemon's next start. A hub that stepped
   down is not closed: its next reload reads both files again and starts if the topology names it the hub. Limit: the
   owner is idle again only when its task's loop ends; an owner task that panicked (no input is known to cause one)
   would leave it running for the daemon's life, and every later reload would wait `SETUP_WAIT` for its end and start
   nothing. A reload costs
   every watching window a resync, as any stop does
   (`chat_watch_tests::a_watch_through_a_reload_is_told_and_its_next_watch_is_served_by_the_new_service`). The hub fact
   is read at the start and once a minute after (`HUB_CHECK`). A running chat steps down when a readable topology names
   another computer as the hub: every workspace's service stops, which frees each lock, and each status is `NotHub` (`names_another_hub`
   is true only for a topology that parses and a self-name that is not empty: a missing or unparsable topology and an
   unreadable or empty self-name never count as another computer naming the hub, and a chat running through one runs on;
   `a_chat_whose_topology_names_another_hub_steps_down_and_a_broken_topology_changes_nothing`). One watch on the hub fact
   runs per chat, not per workspace, whatever the reloads: it asks only while a workspace runs or starts, stops every
   unit when another computer is named the hub, holds the chat weakly between asks and ends with its retirement
   (`one_hub_fact_watch_steps_every_workspace_down`). A step-down opens no file. A start refused `held` is not final: it is tried again
   each minute, logged once, the phase staying `Starting` so the owner's wait holds and the owner starts when the lock
   comes (`a_held_start_starts_once_the_holder_lets_go`). The tries end at a retirement, or when a readable topology names
   another computer as the hub (logged: the chat is off, status `NotHub`); a missing or unparsable topology or an
   unreadable self-name ends nothing, so a `hosts.toml` edit in progress cannot leave the chat down until a restart
   (`a_held_start_survives_a_topology_that_does_not_parse`). A held start holds its chat: the registry owns the chat until
   retirement, and the retirement's abort ends the tries; dropping the chat's handles does not. Any other failed start is final until a reload or a daemon restart. **The owner.** The daemon's boot is `Chat::boot` (begin, the boot's log line, and `Chat::start_owner`, called while the chat is still `Starting`; the tests' rig is built by the same function): one task per workspace waits for its start to end and, when its service runs, logs `the chat service is running` once with the workspace (and `Service::person_dropped`'s code when the start dropped the user token), keeps the workspace's name (`Chat.names`, the last name Slack gave this daemon for each admitted workspace, kept past its service's end and pruned at each load, which `chat.workspaces` reads), and calls `Hub::ensure_owner`, so the socket is served with no window open; `ensure_owner` stays the one start, and a first watch that comes earlier starts it too. The wait ends with the chat when it is retired while starting. An owner ends with its service (every watch told to resync) and the hub's owner is idle again, so a reload's service gets one of its own; **a service that ended on its own** (`Service::ended`, guarantee 22) ends its owner the same way, and the owner, as its tail, begins the chat's stop (`Chat::service_ended`: the phase is `Stopping` and the unit logs `project chat: a workspace's chat service ended` with the workspace and the code alone, at most once: a stop, a step-down or a reload that began first logs nothing of it), returns the delivery's filing in flight (no stop asked it to; `chat_mail_tests::a_filing_in_flight_at_an_end_returns_before_a_reload_starts_the_next_service`), drops its receiver of the stop's request, waits at most `2 * STOP_WITHIN` until the chat is `Off(Failed(code))`, and only then tells its watchers, so a window that resolves again at that event reads `unconfigured` (`chat_watch_tests::the_chat_is_off_with_its_code_before_a_watcher_is_told_the_service_ended`, 60 rounds on a multi-thread runtime). `Hub::ensure_owner` does the same turn-off when the ended service gives no `Watches` (the end came before any owner took them, so no owner would see it; `an_ended_service_no_owner_saw_is_turned_off_when_an_owner_is_started`), so every caller of it is covered. `service_ended` acts only on the service the chat runs. The status stays off until a reload starts the next service, a connect or a daemon restart (`an_ended_service_turns_the_chat_off_with_its_code_and_frees_its_lock`). Limit: the stop's order (the listener's lock outlives the last filing) is a stop's; an end frees the lock first, with the listener's task, and the filing in flight returns after it (up to `MAIL_END`), so a second daemon's start on the same comm folder can overlap that filing, and until the filing returns the chat reads `Stopping` (`chat.resolve` the mapping, not delivering). The boot's task logs `bots.info`'s failure once, with `Service::app_unchecked`'s code, when the service runs; a reload starts its service only after the old owner ended, or `ensure_owner` would find it running and start none (`chat_mail_tests::the_owner_runs_after_a_reload_with_no_watch`: a delivering file files a live post after the reload with no watch). An owner with nothing watched applies nothing and reads no history (`the_owner_runs_from_the_services_start_with_no_watch` with a fake Slack that counts history reads, `the_running_state_is_logged_once` through `test_log`, `the_boots_wait_ends_when_the_chat_is_retired_while_starting`). The rig's tests run with the boot's task, and either start may come first (`ensure_owner` holds the owner's lock); the tests built with `begin_unowned` (`many()`'s: `a_watch_is_admitted_and_bounded_per_connection`, `a_closed_connections_channels_leave_the_model`) have no boot task, so a first watch starts the owner there. `begin` is private to the daemon's chat module, so `server::run` can build a chat only through `boot`. **The names of the mapped channels.** When the service runs, `start_owner` also spawns one task, detached, that calls `Api::channel_names` on the configuration's channels, so `chat.resolve` can carry each mapped channel's name, public or private, from the cache alone (`Api::cached_name`, whose names all went through `clean`, so none is empty or past `MAX_NAME`; a resolve makes no Slack call and answers at once). The lookup is held to what a fill's is: the distinct channel ids among the mappings, at most `MAX_CHANNELS` (a user or direct-message id is not asked for), one after another inside one `NAMES_DEADLINE`, ended at the first answer no other channel escapes (`gives_up`: a missing scope or a rate limit); a name already cached is not asked for again. `channel_names` returns each channel it tried that got no name, and each the deadline cut off, with a fixed code (`ApiError::code`, so `slack` for a missing scope and `malformed` for a reply with no usable name, or `timeout`); the channels a stop at `gives_up` or the cap left untried are not returned. The daemon logs each as a warning, the code and the channel, never Slack's words, and no test reads the line. Limits: it runs once per service start (the boot's and each reload's), so a failure or a rename shows after the next start; the names are the oldest entries of the 1,024-entry name cache, so a busy hub can push them out, and a window's title then shows the id after its next resolve; a resolve that comes before the lookup ends carries no name, and the window asks again on a short schedule (the fe-ui charter's `RowChannel`). The lookup never holds the start of the owner, the first watch or mail delivery back (`chat_watch_tests::a_name_lookup_that_never_answers_delays_neither_the_owner_nor_the_first_watch`). **The delivery's owner (`rust/backend/src/chat/watch.rs`; built).** On a hub whose `chat.toml` sets `mail_after`, each workspace's owner opens a `Delivery` at its start for every mapping of its workspace with a floor: each channel's `State` is loaded from `<daemon state>/chat/delivery-<workspace>-<channel>` (a file that cannot be read stops that channel, logged as `a delivering channel's state is unreadable` with the `StateError` code), and `Watches::deliver` makes the model hand its records over. The start's read of the inboxes (`read_filings`) runs on the blocking pool and is tried again on the backoff while it fails; `Delivery` files and passes nothing before it succeeds (`an_unreadable_inbox_at_start_files_nothing_until_it_reads`). Each handover of a pump or a `begin` goes to `Delivery::hand_over`; the owner asks `poll` after every wake and runs a `File` as a task (the author named by `fill_names`, then `mail::file`), one filing in flight, and gives its verdict back. Every workspace's filings take the daemon's one filing slot (`FilingSlot`, C8: one filing at a time, `FILE_GAP` after the daemon's last filing returned, so a registry writer of another process meets free polls after every filing's hold whatever the number of workspaces; a filing still waiting for the slot when its delivery ends has filed nothing and is dropped at once, so `MAIL_END` is never spent behind another workspace's filings; `chat/watch_tests.rs`: `the_daemons_filings_take_one_slot_file_gap_apart`, `a_dropped_filing_waiting_at_the_slot_files_nothing`; `chat_mail_tests::two_workspaces_file_their_own_mail_one_filing_at_a_time`, `a_filing_waiting_behind_another_workspaces_ends_at_once_with_its_delivery`; a filing's name lookup, a Slack call of its own workspace's, runs before the slot is taken, so a slow Slack in one workspace never holds another's mail). One log line per filing carries the channel, the `ts`, its source (`live` or `pass`) and the refusal's code alone, never its sentence or any text; the start's read logs the filings it found per channel. A post filed live is not filed again after a restart, a moved hub or an emptied state folder, since every start reads the inboxes (`a_restart_after_live_filings_files_nothing_again`). A file without `mail_after` opens no delivery, reads no inbox and files nothing (`a_file_without_mail_after_reads_no_inbox_and_files_nothing`). A filing job that panics or is cancelled answers `job_failed` (logged), so the one filing in flight always returns (`a_filing_job_that_panics_still_answers_and_the_next_post_is_filed`). **Passes and the commit.** A `Pass` the delivery asks for runs as a job: the recipient is resolved first on the blocking pool (an absent one answers `Skipped` with its code, `no_row`, `ambiguous_row` or `no_live_session`, and costs no read, so the delivery owes the pass again on its backoff), then the job reads the clock for the pass's upper bound U (after the recipient's hop, not before) and calls `traverse::pass` with U and the pace; the `Passed` it returns goes to `Delivery::passed`, and one log line carries the channel, the outcome, the reads, the number of mail records, whether Slack limited the channel and the threads it called not found. A job that panics or is cancelled answers `Skipped("job_failed")` the same way a filing's answers `job_failed`, so a pass in flight always returns (`run_job`; `a_pass_job_that_panics_answers_skipped_and_the_next_pass_runs`). An absent recipient costs no read, and a live post it refused is found once by the next pass (`a_pass_with_no_recipient_reads_nothing_and_a_refused_post_is_found_once_the_row_returns`). A pass that fails keeps and hands over what it read, oldest first as a complete one does (`incomplete_recovery_keeps_its_candidates_and_reports_it`); it read the newest pages, so a later pass can file older mail after it. The pass does not ask `names_another_hub`, by decision: a deposed hub steps down within `HUB_CHECK` (a minute), which ends its delivery and its passes, a pass writes nothing, and each filing asks the hub fact first and files nothing (`other_hub`), so a second check would add a topology read per pass for a rule the step-down already gives. A wake's characterization (`comm_wake::a_chat_line_wakes_a_free_claude_row_with_the_fixed_line_alone`) proves a `#chat` line, filed through `comm.file`, wakes a free Claude row with the fixed line alone and nothing of its hostile body. A post made while the socket was down, a live post whose filing was refused, and a post another connection on the app's token took (D8's second socket) are found by the first pass after the next hello, a refusal or the 6 h sweep (`an_empty_state_folder_after_live_filings_files_nothing_twice` and its siblings run the passes against the fake's measured history). **The commit is not answered:** `Delivery` has already moved the boundary in memory, a write that fails leaves the old file, which only costs the next start reads (a stale file is safe), and nothing waits on the write, so the blocking task that calls `mail::commit` logs its failure and returns no arm to the owner; two commits of one channel are two tasks that write one temp name, so one that lands late can leave a lower boundary on disk and one can fail (a logged warning), the same stale file; commits of a channel are at least a pass's pace apart, so it takes a stalled disk. The passes are proved with the fake Slack's `History` (the pages as SlackClaw measured them): a pass over a channel of 50,000 roots reads it in at most 53 reads, moves the boundary and files a reply under a thread begun at the floor's end (`a_channel_of_50000_roots_completes_its_passes_moves_the_boundary_and_files_an_old_threads_reply`), and the start's read of the inboxes is what keeps a pass from filing again what an emptied or older state folder forgot (`a_state_file_older_than_the_inboxes_files_nothing_twice`; the unreadable-inbox case files and passes nothing until the read succeeds, then the first pass files nothing the inbox holds). **The end order.** `Chat::stop_unit` (a step-down to `NotHub` or a retirement to `Stopped`, every unit's begun at once so lifecycle's bound holds for all: `retirement_stops_every_workspace_within_its_bound`) calls its workspace's `Hub::end_mail` before `Service::stop`: the owner takes its delivery out of use, waits for the filing in flight to return (at most `MAIL_END`, 15 s: a name lookup, bounded at 10 s, the guarded append, bounded at about 3 s, and the recipient and topology reads, which have no bound of their own, so `MAIL_END` is the only bound that holds), ends its pass and the start's read (`run_job` ends its job when its task is dropped: `a_pass_in_flight_at_a_step_down_ends_with_the_delivery`; the read ends itself at its next turn once the owner's receiver is gone, which no test can tell from the abort) and answers the stop's request on a channel of its own, so an owner that is gone, or dies unanswering, ends the wait at once (`ending_a_delivery_whose_owner_is_gone_returns_at_once`), so the listener's lock, which keeps a second computer's daemon from filing, outlives the last filing (`a_step_down_returns_the_filing_in_flight_before_the_service_stops`). A daemon with no delivering channel answers at once. The retirement's bound, `RETIRE_WAIT` (1 s), is shorter than `MAIL_END`: a filing in flight that outlasts it is reported honestly, `chat` among the retirement's `incomplete`, the daemon ends regardless, the process exits at that bound and its death frees the lock (the service stops later only in a test, where the process goes on) (`a_retirement_waits_for_the_filing_in_flight_and_reports_itself_incomplete_when_that_outlasts_its_bound`). While the stop waits the phase is `Stopping`, so chat requests answer `unavailable` and the owner, parked in the wait, drains neither the socket nor the windows' snapshots; they are told when the service stops. A filing killed by the daemon's own exit is in no inbox, so the next start's read does not see it, and the first pass after that start finds and files it. `Chat::boot` takes the delivery's `Timing`; the daemon passes `Timing::PRODUCT`. **The mail jobs (`rust/backend/src/chat/mail.rs`).** Each is blocking work the owner runs off its loop, and answers what `Delivery` asks for. `recipient(rows, folder, env, now)`: of the rows whose canonical folder is the mapping's folder and whose `bound_handle` is not empty, the one Claude row, else the one Codex row (the inert anchor runs no agent, so it is neither); absent, with a fixed code, as `no_row`, `ambiguous_row` (two of the kind chosen) or `no_live_session` (the filing's own admission, `filer::admit_entry`, refuses the handle: not listed with a host, bound to another row, or a heartbeat 600 s old or more; so the recipient and the guard share one predicate); a Claude row whose session is gone is absent, never replaced by a live Codex row (`a_handle_with_no_live_session_is_absent`). `file(env, rows, folder, record)` asks `names_another_hub` first and, when it is true, does nothing else (`other_hub`: the window of the hub's move); then the recipient; then `append`, the one filing through `file_comm` from `#chat`, which names the row it expects, so a handle moved since the recipient was read is refused (`not_here`) under the registry lock and nothing is appended. `Ok` is `Verdict::Filed`; an `Err` is `Verdict::Refused`, its text what the owner logs: a fixed code, or the filing's own `code: sentence` (`file_failed` alone would not say a held lock from an unreadable registry); a Codex row gets mail and no wake. A const assertion holds `FILE_GAP` at four registry-lock polls or more. `commit(dir, workspace, channel, boundary)` makes the state folder private and writes the boundary durably; a failed write returns its code, which the owner logs before going on, and the old file stays, which is safe (it costs the next start reads). `read_filings(env)` is the start's read (D10): every inbox's `#chat` lines through messaging's `lines_from`, read back by `mail::filed_of` into one flat list, which `Delivery::start` hands to each channel's `State::merge`, which keeps its own workspace's and channel's; any fault is the `Err`, never no filings, so the owner tries again and files and passes nothing until it succeeds. **M3, `Chat::admit`, `Chat::admit_unwatch` and
   `Chat::resolve`.** `Chat::service` is private, so a call that reaches Slack or a file gets the service only from
   `admit`, which reads the registry's current entry for the row id at that moment (an exact id, never a slug) and
   refuses: no running service of the target's workspace (not the hub, no or refused `chat.toml`, no secrets, a failed, starting, stopping or
   stopped service) as `unavailable`; a removed row, a row replaced under a new id, an id that never existed, an empty
   one and a folder that no longer exists as `stale_row`; the inert anchor, and a folder the file does not map, as
   `unmapped`; a folder mapped to another workspace or channel than the request expects, or an expected channel the
   file does not hold, as `stale_mapping`. A new row in the same folder does not inherit a request that named the old
   id, and a request naming the new id works. `admit_unwatch` hands out no service and needs only the target's workspace's running one: an
   unwatch calls nothing, and refusing it for a stale row would leave the watch holding one of the bounded slots. The
   mapping is the configuration of the last load (the boot's or a reload's). Tests: `stale_row_or_mapping_cannot_post_or_fetch` and
   `no_action_is_admitted_unless_the_service_runs` prove a refusal by no service being handed out, and the backend's `server/chat_ops_tests.rs` proves it through a connection by the fake's record holding no history, replies, `chat.postMessage` or `users.info` request (the listener's own `auth.test` and `apps.connections.open` are in the record and are not counted); that `service` is private rests on its visibility and review, no test can see it (they are a
   child module). `resolve` asks the hub fact first, then the row, then the file: `NotHub` for a non-hub whatever the
   id, `stale_row` for a row that is gone, `Unmapped` for the anchor; otherwise the file's mapping in every service
   status, with `delivers` only while the mapping's workspace's service runs (starting, failed, secrets missing, stopping and stopped show
   the mapping and do not deliver) and `Invalid` for a refused or missing `chat.toml`
   (`resolve_joins_the_mapping_with_the_row_and_the_service_status`,
   `resolve_asks_the_hub_fact_first_and_the_row_before_the_file`). The op `chat.resolve` (`chat/ops.rs`) answers from
   `Chat::resolve` with one more state, after the hub fact and the row: `unconfigured` when the chat a connect would fix
   is off, judged for a mapped row by its own workspace's status and for an unmapped row only when no workspace's
   listener runs (C4, so one workspace whose tokens failed never keeps a person from pairing in another;
   `chat_setup_tests::resolve_judges_a_row_by_its_own_workspace_and_an_unmapped_row_by_whether_any_runs`); a mapped
   row's `unconfigured` names its workspace, the one whose connection failed, so the window says which and offers the
   others (`two_workspaces_from_nothing_each_project_reads_and_posts_in_its_own`)
   (`setup::unconfigured` of the status: no `chat.toml` or no
   secrets `not_connected`, secrets the load refused `tokens_refused`, a failed start `start_failed`, a service that ended on its own `wrong_app` for an app token of another app and `tokens_refused` for a token Slack refused, `app_token_refused` or `bot_token_refused`); a refused
   `chat.toml` stays `invalid`, since a connect never rewrites it
   (`server/chat_setup_tests::resolve_says_unconfigured_for_each_reason_a_connect_fixes`, `resolve_says_why_a_service_ended`, guarantee 31). Limit: the check is at admission and holds no rows
   lock across a Slack call, so a call already made, a history page's name lookups included, is not recalled when its
   row is removed afterwards. The request ops (`chat/ops.rs`) call `admit` on the blocking pool, since it canonicalizes on the shared home (rests on review: no test can make a folder lookup slow). `chat.watch` (`chat/ops.rs`) and each snapshot attempt the watch owner makes (`chat/watch.rs`) are admitted again through `admit` when made, so a watch whose row went away is refused and its slot released; `chat.unwatch` calls `admit_unwatch`.
   **Who posts as whom (D13).** After admission, `chat.post` and `chat.post_file` (one function, `ops::may_post`) refuse two more things as `bad_request`, before any Slack call and with no wire change: a `person` post from a connection that is not a window's (`Shared.window`: the hello's role `fe` with a name), which keeps a row from posting a `Drawer` post, which is mail, to itself; and a `row` post whose `name` is not the target row's bound handle (`bound_handle`, rows', compared exactly). A refused post's code is by its kind: a person's `not_in_channel` is `not_a_member` (the token is kept) and a code that names the token (`api::refuses_the_token`: `TOKEN_REFUSALS` or `missing_scope`), which the API has already forgotten the person's token for, is `no_user_token`, as is a person's post with none held (`NoPersonToken`, unsent); a row's `not_in_channel` stays `failed`, since there it means the app is not in the channel. `chat.resolve` says nothing of the user token (a row's `sot-fe chat post` resolves its pair first and posts only when it is mapped, and a row needs only the bot token), so a hub that holds none answers every caller as it always did, and it is a person's refused post, `no_user_token`, that brings the window's Connect step (`server/chat_setup_tests::a_hub_whose_secrets_hold_no_user_token_says_so_and_posts_nothing_for_a_person`, which resolves and posts as a row's connection does). A person's `chat.post_file` from a window reads its file under the rule a row's does and is shared under the person's own user token (`chat_ops_tests::a_persons_file_post_from_a_window_goes_under_their_token_once`). The role is the client's own declaration, so any local process of the account can pass both: attribution among one account's programs, not a security boundary (channel members cannot forge `Drawer` or `Row`, guarantee 2). A row replies through `sot-fe chat post` (`agents/CLAUDE.md`). Tests: `chat_ops_tests::row_verbs_target_current_thread_and_submit_once`, and step 6 of the stage test `chat_mail_tests::with_the_window_closed_a_channel_post_becomes_marked_mail_and_the_rows_reply_reaches_its_thread`, which runs the whole path with the window closed.
   **A row's file (`chat.post_file`).** After those two refusals the daemon takes a transfer permit (`Api::transfer`, guarantee 32; `busy` at once while `MAX_TRANSFERS` are held), so no file is read into memory without one, then reads the file itself, once, on the blocking pool, before the one `Api::post_file`: the node id composed beneath the row's root by the files read resolver (`FilesMode::node_id_to_path`: `..` and absolute parts refused, links followed, so a `data/results` link to another disk posts) and opened as a regular file only (`open_regular`); then the opened file is judged, never a name: on Unix it is refused when its (device, inode) is the configuration folder's (`config_dir`) or any of its entries' (the secrets file above all), so a link, a hard link, a bind mount, a swap of a folder before the open and any spelling of the name fail alike, and a folder that exists but cannot be listed refuses every file; only the size the opened file states is read, into a buffer of that size: 0 (an empty file, or a kernel file such as `/proc/self/environ`, whose stated size is 0) is `not_a_file`, more than `MAX_CHAT_FILE` `too_large`, and a file that gives fewer bytes than it stated `not_a_file`. Not a readable regular file is `not_a_file`; each refusal is an error reply before any Slack call. The share's title and filename are the node's own base name (a link's name, not its target's). The outcome maps as a post's: `posted {file, ts?}`, `refused` (`busy` from the HTTP core, `bad_request`, `unavailable` for a client closed, or `failed`), `limited`, `unknown`, never resent. Limits: on Windows, where std has no stable file id, the opened name's canonical path is compared with the folder instead, so a hard link or a swap there reaches the secrets; entries in a subfolder of the configuration folder are not compared (the chat's files are at its top); a file on a hung network mount holds its blocking thread with no deadline of the daemon's, and the tool gives up at 120 s with exit 3 though nothing was sent (the post happens when the read returns). Rows share the daemon's account, so the rule is attribution and accident, not a security boundary: a row can read the file itself. Tests: `chat_ops_tests::a_row_posts_a_file_of_its_folder_under_its_own_handle_once` (D4's refusals, the stale row and mapping, once, `unknown` never resent, a refused first step `refused` with steps 2 and 3 never sent, a limited share `limited`), `a_file_outside_the_folder_or_of_the_config_folder_is_refused_before_slack` (the spellings, a hard link, a folder swapped for a link to the configuration folder just before the open through the test seam `ops::BEFORE_OPEN`, `/proc/self/environ` on Linux, the bound; a link to a file of another name posted under the link's name), `a_third_file_post_at_once_is_busy_before_its_file_is_read`, and goal 1's `chat_mail_tests::a_rows_figure_reaches_its_thread_once` (the three steps once, the bytes unauthorized, the share read back as the row's and not mail). The link, hard-link, swap and FIFO cases run on Unix only. `chat.file` is admitted the same way and calls `Api::fetch_file` once, which takes its own transfer permit (guarantee 33), so the daemon takes none; the picture's bytes are the reply's blob (`server::chat_ops_tests::a_file_reply_carries_its_bytes_as_its_blob`).
   The listener's own calls (`auth.test` at start, `apps.connections.open` for each socket) are Slack traffic made with no admission: they name no row and act for no request, so M3 governs only what a request names; the listener makes no name lookups. A row made after the start in a mapped folder is admitted at its first action (`admit` and `resolve` read the registry's current
   entry), and a mapping to a folder no row holds refuses nothing (`a_mapped_folder_with_no_row_at_start_leaves_the_other_projects_mapped`,
   `a_row_made_after_the_start_in_a_mapped_folder_is_admitted`). A second daemon of the account
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
22. **One workspace has one listener, and its lock lives exactly as long as the listener.** `Service::start(workspace,
   credentials, http, dir, transfers)` takes the workspace's lock first, then reads `auth.test` (its `team` kept as the
   workspace's name, `Service::workspace_name`), then asks `bots.info` for the bot's app (its
   `bot_id`; the answer's `bot.app_id`, an id, is `Identity.app_id`, which the listener holds every frame to, guarantee
   17), then opens the socket. `bots.info`'s failure, whatever its code, ends nothing and leaves the app unknown, so no
   frame's app is checked; `Service::app_unchecked()` says why with the failure's `ApiError::code` (`no_identity` when
   `auth.test` named no bot, asked nothing) for the daemon to log (PS8: a wrong premise costs the check, never a
   working chat); a second service for the
   workspace is `ServiceError::Held` before any socket opens or any call is made. The lock is platform's `lock_supervisor`
   (a kernel `flock`, close-on-exec) on `<dir>/chat-<workspace>.lock`, a name built from the validated workspace id alone,
   holding no secret; `dir` is a private folder the caller supplies and owns (not checked here); the daemon's is the comm folder's `chat/`, so
   every daemon that files into those inboxes takes the same lock and one listener runs per comm folder
   (`two_daemons_on_one_comm_folder_run_one_listener`). The lock is moved into the
   listener's task (`Socket::launch`'s `held` argument), so it is released when the task's future is dropped, after its last poll: at a
   normal end, at a `stop` that timed out and aborted (the abort is awaited first), and at a drop without `stop` (the
   abort, not an await). A killed holder releases it with its descriptor, a child born later does not inherit it, and
   `start` after `stop` in one process is admitted. **A service ends on its own, for good**, at the first of: its
   listener's `wrong_app` (guarantee 17) or `app_token_refused` (guarantee 20), or a bot-token call (a read, a row's post,
   a join, a row's file step) Slack answers with a code of `api::TOKEN_REFUSALS` in its body (`bot_token_refused`, premise
   PS11, guarantee 20; `missing_scope`, every other code, a bare HTTP status such as 401 or 403, a person's token refused
   (guarantee 12) and `bots.info` end nothing). `Service::ended()` is that
   first code. An ended service answers as a stopped one: `api()` is `Stopped`, every handle `Closed`, its `Watches`
   reads every channel `Needed`, `wait` answers `Stopped` (one in flight is woken), `take` gives nothing and `watches`
   none; its status is `Stopping` until the listener's task is gone and `Stopped` after, and the lock is released with
   the task, so a second service of the workspace starts. A client closed by a stop answers `Closed`, which is no end.
   The claim is the one arbiter: a stop marks it first, under the phase lock, and an end sets its code only when its
   swap of the claim is the first to end it, so a stop begun first leaves `ended()` `None` and a second end never
   replaces the first. The crate logs nothing: the codes are values the daemon logs (the owner's end and `ensure_owner` read `ended()`, the boot's task `app_unchecked()`, guarantee 21). Notes, not fixed:
   `app_unchecked`'s code is `ApiError::code`, so every Slack refusal of `bots.info` reads `slack` and the daemon's log
   cannot tell `missing_scope` (no `users:read`) from another; and `End` and the codes live in `service.rs`, which
   `api.rs` and `socket.rs` now import while `service.rs` imports them, a module cycle where `service` was the top (an
   `end.rs` would undo it). A token whose `auth.test` names another workspace is
   `Api(wrong_workspace)` before serving, and the lock is released. `stop(within)` is bounded and final: the signal, a join
   within `within`, then abort and await; a second call returns at once, and `api()` is `Stopped` and every handle already taken answers `Closed` (the API itself is closed, before the socket is
   signaled) from the moment a stop begins; only the call that began the stop waits for the listener, any other returns at once;
   a request already in flight (including `apps.connections.open`, whose ticket nobody connects with) runs to its own deadline. `status()` is `Running { socket, gap }` (the socket's `Status`; `gap` is true after any hello until `Watches::reconcile` clears it through `Service::reconciled`; `Service::gap` is a status read; `Watches` takes the events, answers `loaded`, clears through `reconcile` and waits, and all of them answer nothing once a stop has begun; a `wait` already in flight is woken by the stop and answers `Stopped`),
   `Stopping` or `Stopped`. A service holds four things: the workspace lock, its phase, the API's closed state (set when a stop begins, never cleared, seen by every handle), and the claim that makes `Service::watches` give one `Watches`: free, held while that `Watches` lives, and ended when a stop begins (or the service is dropped), which is never undone, so a stopped service gives none and its `Watches` reads it. A stop whose caller stops waiting (its future is dropped, as lifecycle's 1 s retirement bound does) leaves that service at `Stopping` for good, not `Stopped`: every handle answers `Closed` and the task is aborted, so the lock frees when the runtime drops the task and a `start` right after may wait platform's short retry; nothing may poll for `Stopped` after cancelling. A service refused or never started has no value, its state is `start`'s error. The error codes are
   the existing ones plus `held`, `lock`, `stopped`, and `closed`, which a handle answers once its service has begun to stop. Limits: a second computer sharing the home over NFSv4.2, where `flock` is
   a byte-range lock on the server: measured, a lock held on one computer refused `flock -n` on another, and a holder
   killed with `kill -9` freed it for a second computer on its first try, 0.23 s after the kill; a lost computer is not
   measured and rests on the NFS lease (120 s on the lab's mounts), so its lock should free within about two minutes; a
   holder cut off but still running does not learn its lock was lost, so a hub moved during a cut longer than the lease may
   file one more message from the one filing it had in flight, and every filing
   reads the moved topology first and files nothing. A mount that does not support locks is not measured, and Windows is `LockFileEx` with no cross-computer
   claim. A comm folder outside the shared home gives per-computer exclusion only. A duplicate app id across different
   workspace ids is not detected; an app token of another app than the bot token's is detected by the listener at the
   first `hello` or envelope that names its app (guarantee 17), not at the start. A
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
   it clears at once. A delivering channel (`deliver`, which refuses an invalid id as `watch`
   does) is no watch and never keeps the gap. Every `New` record of one taken from the socket, root or reply, any
   origin, is handed back by the `pump` or `begin` that took it (`Arrived`, `#[must_use]`), once for each time the socket
   gives it, in take order (not `ts` order), with the highest gap number seen, which only rises; the model keeps no
   handover, so none outgrows one take (the socket's own 256 events), and a stopped service hands nothing over because
   `Service::take` returns nothing once a stop has begun. The set's 32 is the configuration's refusal alone
   (`TooManyDelivering`); the model holds a set of ids and enforces no bound. One total order on timestamps
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

24. **A channel's message becomes one bounded, marked mail body, read back by one reader, and only people's posts are
   mail.** `mail::body(record)` is pure and returns at most `MAX_MAIL` (12,000) bytes for every record this crate builds, which is
   within the record's own limits (a hand-built record with fields past them can exceed it), under messaging's poll
   budget (`POLL_BUDGET`, 16,000, in `comm::cli::poll`) so one chat mail is printed whole. Its first line starts `[chat] `, names the role first (`a channel member`, or `the
   chat drawer` for a `Drawer` post, which only an older daemon made, as the app, for a window) and then the author's id and quoted name, the channel and
   `at the top level` or `in the thread of <ts>`, and says that lines starting `| ` are the channel's words, not the
   user's. Its second line is `ts=<ts> thread=<ts> channel=<id> workspace=<id>`, ending ` cut` when the record's
   `truncated`, `omitted` or `files_omitted` is set or the mail dropped any text or file, so ` cut` says that
   something is missing, whatever the reason. Then each line of the text, an empty one included, behind `| `, then
   one `file:` line a file (name, MIME type, size, `not fetched`; a file's bytes are never fetched here) and
   `and <n> more files` for the files the record itself left out (past the eighth, or an entry that could not be
   summarized) and any that did not fit. The text is read once through wire's
   `text_segments` (tokens and words decoded, a token shown as Slack sent it, `\r\n` as `\n`). **Every field taken from
   the record passes through one escape**: each character wire's `unsafe_char` lists (every control, so a lone CR and
   what a terminal acts on, and the format characters it names: U+2028 and U+2029, direction marks and the zero-width
   and joiner marks) is shown as `<U+XXXX>`, and in the quoted name, a file name and a MIME
   type so are `"`, `(`, `)` and `,`; so no post can forge a poll line, a header, a file line, a ts line or a wake line,
   close its quote, pose as a role, or reach a terminal as a control character. Over the bound the files that cannot fit
   beside the header become the count (each file count is measured with the mark it will carry, so a body that fits
   shows every file and none passes the bound), then the text is cut once, after escaping, to the longest prefix that
   fits (never inside a character or an escape). Files come before text because a file line is bounded and the only
   place an attachment's name, type and size appear, while a cut text keeps its first part. Limit: tag characters (the U+E0000 block), U+00AD and U+180E are not in
   `unsafe_char` and pass through, so a body can carry text a person reading it does not see
   (`PAGE-REPLY-UNSAFE-CHAR-PARTIAL`, wire's). `mail::filed_of(body)` reads the workspace, channel and `ts` of a body from its first
   two lines alone, in exactly the form `body` writes, each id as `config` reads one (`config::is_slack_id`: a capital letter, then capitals
   and digits, at most 32, so a record whose ids are not Slack's is never read back); a line of the text is behind
   `| ` and is never the second line, so a post cannot forge a filing. `mail::is_mail(record, mail_after)` is true for
   origin `Member` or `Drawer` with both `ts` and `thread_ts` after `mail_after` (`ts_key` order); a row's own post, a
   bot's and another app's are never mail, and a reply in a thread begun at or before `mail_after` is not. Limits: an
   edit or a delete is not told by the record, so the caller (the watch model) hands over new records only; a name,
   a file name or a MIME type is shown as the escape leaves it, so a hostile name is readable only as `<U+XXXX>` marks
   inside its quotes; nothing here logs, files, or reads a clock.

25. **A delivering channel's boundary is one small file, its filed set is in memory, and the boundary is never taken from a
   filing.** `state::path(dir, workspace, channel)` names `delivery-<workspace>-<channel>` in the caller's folder from two
   `is_slack_id` ids alone (any other id is `BadId`, so no raw configuration string reaches a path). The file is one `Ts`
   and its line end, exactly (`encode`, `decode`: a missing line end, a second line, an empty or non-`Ts` line is
   `Unreadable`), written whole by `state::write` through platform's `write_durable` (no temporary file of this crate's)
   and read by `State::load` through `read_regular` at no more than 4 KiB. `load` answers the later of the file's boundary
   and the channel's `floor` (passed in; `ts_key` order), a missing file is the floor, and any other failure to read (a
   refused file, a directory, a FIFO, over 4 KiB, not UTF-8, not one `Ts`) is `Unreadable`, never read as missing, so the
   caller stops that channel. `State::commit(to)` never lowers the boundary and drops every filing at or below it. The
   filed set is held in memory only: `merge` takes the filings the caller read back from the inboxes (`filed_of`) that
   name this workspace and channel and lie above the boundary, `insert` takes one that returned `filed`, and
   `contains(ts)` is true for every `ts` at or below the boundary (below it every post was filed or passed) and, above
   it, for every one held, so a caller has one check. **The bound is soft:** `MAX_FILED` (100,000) never drops a
   filing above the boundary, because a dropped filing would be filed again by the next pass and the start's read holds
   those lines in memory already; `merge` and `insert` return true once, when a call takes the set past it, so the caller
   logs it. No filing moves the boundary. Limits: `load`, `write`, `merge` and `insert` do no logging, and the folder, the
   inbox reads and the clock are the caller's; a link at the file's name is followed on read, as `read_regular` does (a
   link to nothing reads as missing, and a link to another file gives that file's boundary), which takes this account's
   own hand in a private folder; a write that fails leaves the in-memory boundary where `commit` put it, so the caller
   writes `boundary()` again; `read_regular` ends a FIFO's wait only, as its guarantee says.
26. **A pass walks one channel back from its upper bound to its floor, and each thread with a reply after the
   boundary back to the boundary, in fresh reads, and files and commits nothing.**
   `traverse::pass(api, order, upper, pace)` takes an `Order` (a snapshot of the channel's `State`, its floor, and the
   greatest `ts` the delivery was handed for it before the first read), the pass's upper bound U (the caller's clock,
   passed in) and the pace between reads, and returns `Passed { horizon, mail, outcome, reads, limited, skipped }`. Every read is
   `Api::scan` at 999 with `oldest` = the floor, and `latest` = U on the first read and the oldest raw `ts` of the page
   before on each next one (the walk guarantee 11 records with its measurements), so a page of joins or tombstones moves it
   too. It ends at the first page that says `has_more: false`, full or not, never at an empty read after it, and reads on
   past a short page that says more is behind it (`has_more` decides, not the count). A page that does not lower `latest`
   ends the pass `ReadFailed` instead of reading it again. After each channel page, every parent on it (whatever its
   subtype, a tombstone included) whose `latest_reply` is after the boundary has its thread read, when the boundary is
   below U: `Api::scan` of the thread at 999 with `oldest` = the boundary and `latest` = U on the first read and the
   oldest raw `ts` of the page's replies on each next one, the parent, which a replies page holds on top of its limit and
   alone on the otherwise empty last one, never the next `latest` (the walk guarantee 11 records, measured at limits 2
   and 999 and with posts arriving). A thread ends at its first page with `has_more: false`, as the channel does, and a
   thread page that says more and lowers nothing is `ReadFailed`. A thread read is a read of the pass in every rule here:
   paced by `pace`, counted by `MAX_PASS_READS`, halved and made again when too large or slow, a 429 or `ratelimited`
   ending the pass `RateLimited`, any other failure `ReadFailed`, `is_limited` ending nothing. One answer is no failure on
   a thread read: Slack's `thread_not_found` reads as an empty thread, the walk goes on to the next parent, the pass can
   still end `Complete`, and `skipped` counts each such read. A channel read's `thread_not_found` is no special case (it
   ends the pass `ReadFailed`). A reply under a thread Slack calls not found is never read, which loses nothing that
   could be read; whether Slack serves a deleted parent's thread is unmeasured, so the rule's firing is not shown
   (docs/defects.md: CHAT-DELETED-PARENT-THREAD-UNSERVED, closed). The records of a thread join the one candidate set,
   one record a `ts`: a broadcast reply, which the channel and its thread both list, is one candidate.
   `is_limited` is information only (`Passed.limited`): it ends
   nothing and fails nothing, so Slack's plan horizon is the walk's own bound. A page that passes its response bound
   (`TooLarge`) or the read deadline (`Timeout`) halves the limit, down to 1, and is read again for the rest of the pass;
   at 1 it is `ReadFailed`. Reads are at least `pace` apart, the halved ones included, and no read waits out a
   `Retry-After`: a 429 or `ratelimited` ends the pass `RateLimited` with the wait. `mail` is every record `mail::is_mail`
   accepts against the floor, above the boundary and not `State::contains`, deduplicated by `ts`, oldest first, bounded
   while it streams at `MAX_PASS_MAIL` (200) after the filed set has removed what it holds; one more than that ends
   `Truncated` instead of `Complete`. `horizon` is the lesser of U and (the greatest of `Order.seen` and the first page's
   `ts` and `latest_reply` values, less `MARGIN`, 2 s), and never below the boundary, which wins when it is above U. It
   is fixed from the first page alone, so a later page's `latest_reply` never raises it: a clock running ahead of Slack's
   is safe, since a post made during the walk below U, which no later (older) read can reach, is then never passed by
   the commit. It is the most the boundary may move to; the newest posts lie above it unless U caps it, and are returned
   as `mail` either way. A parent's `latest_reply` was measured above the read's own `latest` (by a fraction of a second),
   which the margin covers and the cap at U covers beyond it. `MAX_PASS_READS` (10,000) ends a runaway pass `TooLong`; a
   closed client is `Stopped`. Every outcome but `Complete` and `Truncated` commits nothing, and keeps the mail already
   read, the threads read so far included. **A `Complete` pass has read the channel from its floor to U and the thread of
   every parent in it whose `latest_reply` is after the boundary, so a caller may commit the boundary to `horizon` from
   it; from a `Truncated` pass, only as far as the mail it handed over was filed.** `Truncated` hands over the oldest
   `MAX_PASS_MAIL` records, so mail past the cap at or below `horizon` is not in `mail`, and a commit to `horizon`
   would put it at or below the boundary for good (the caller's rule, delivery's). What a `Complete` pass can miss, and
   what the horizon can then rise over, is a reply under a parent Slack's plan hides: no page returns that parent, so
   its thread is never read (whether such a thread can be replied in, or its replies read, was not measured). A reply
   under a parent at or below the floor is not a miss: it is not mail (`mail::is_mail` needs the thread's parent after
   the floor). A reply above `horizon` is not passed by a commit, so one made after its thread was read, or above U,
   is left to a later pass or the live event; and a parent whose newest reply is at or below the boundary has no reply
   to read. A read the pass cannot make ends it incomplete rather than passing over a reply.
   Limits: a full page of 999 with more behind it, of history or of a thread (no measured channel held more than 999, no
   measured thread held more than a few replies), is inferred to be the newest 999; a daemon clock behind Slack's by d
   leaves a post made while the socket was down in the last d before the pass to the next pass; a visibility window
   shorter than the gap between a post's `ts` and the next read was not resolved by the measurement (the margin is its
   reserve); a limit halved by one slow read stays halved for the pass, and a path that accepts requests and never
   answers costs about ten deadlines before `ReadFailed`, on every pass; each parent with a new reply costs a read at
   least, which `MAX_PASS_READS` bounds (`TooLong`); the first read of a pass is not paced against the pass before,
   which is the caller's.
27. **A post is filed once, by one queue, one filing at a time; a pass is owed on a schedule; and the boundary moves only as
   far as filings returned.** `Delivery` is pure: `now` is passed in, and it reads no clock, calls no file and owns no
   thread or channel; the daemon's owner asks `poll(now)`, runs what it answers and gives the result back (`filed`,
   `passed`, `pass_skipped`). `Delivery::new(timing, now, channels)` takes each delivering channel's `State` and floor and
   a `Timing` (`read_pace`, `sweep`, `backoff_first`, `backoff_max`; `Timing::PRODUCT` is 1.5 s, 6 h, 30 s, 10 min, and a
   test shortens them); `start(filings)` takes the filings the start's read of the inboxes found (merged into each set,
   `State::merge`), and until it is called `poll` answers `Idle`, so nothing is filed or passed first. `hand_over(Arrived)`
   takes each new record and the gap number (which only rises): a channel that is not delivering is ignored, the greatest
   `ts` taken for a channel, of any origin and in any order, is kept for its pass (`Order.seen`), and anything
   `mail::is_mail` refuses against the channel's floor goes no further. A mail record enters the one queue, keyed by
   channel and `ts`, only when it is not settled (`State::contains`: at or below the boundary, or held in the set), not
   queued and not in flight, so a post taken twice, live or from a pass, in either order or out of `ts` order, is queued
   once. The queue is first in, first out and holds at most `MAX_QUEUED` (512) records of all channels; a live record is
   also not queued while its channel's set holds `MAX_FILED` filings, whereas a pass's candidates are queued whatever the
   set holds (their commit prunes it, so a full set is drained by its owed pass). A live record that is not queued for
   either reason, and one with `omitted` set (its content was not read: its copy from history is the one to file; owes
   nothing if it is already settled), is not filed and owes its channel a pass (the owner sees it as `poll` answering `Pass`). `poll` answers, in this
   order: `Commit` when a returned pass has settled; `File` for the oldest queued record that is still not settled (the
   check runs again as it is issued, so a filing the start's read settled after it entered is dropped unissued), only while
   none is in flight and `FILE_GAP` (200 ms) has passed since the last filing returned, whatever its verdict; `Pass` for
   the channel whose pass is due, when none is running; otherwise `Idle`, with the instant to ask again. A pass is due
   when a gap number above the one the channel's last completed pass began at has been seen (the first hello owes each
   channel its start's pass, and one gap owes one pass), when a filing was refused, a live record dropped or omitted, or
   a `Truncated` pass completed, or by the sweep, `Timing.sweep` after the channel's last completed pass whatever
   happened, and never before the channel's own backoff has run or before `read_pace` after the last pass's result, whichever
   channel's (the first read of a pass is not paced against the pass before, which is this). A `Pass` carries the channel's `Order` and the pace; the
   caller resolves the recipient first and reads the clock for the pass's upper bound, and an absent recipient is
   `pass_skipped`, which reads nothing and owes that channel's pass again on its backoff. A `ts` joins the set only when its
   filing returns `Verdict::Filed`; `Refused` (any reason it was not appended: no row, an ambiguous one, a moved handle, a
   topology that names another computer) owes the channel a pass, leaves the rest of the queue and does not retry the
   post (it is out of the queue, so only a pass or another event finds it). `filed` and `start` return true when a set
   went past `MAX_FILED`, so the caller logs it. **A pass's result** (`passed`): its mail joins the queue whatever the
   outcome. A `Complete` or `Truncated` one is completed (the channel's gap number and sweep clock move, and the backoff
   resets once its commit is clean); any other outcome (`ReadFailed`, `RateLimited`, `TooLong`, `Stopped`) commits nothing
   and owes the channel another pass after its own backoff, which starts at `backoff_first`, doubles with each failure
   (a failed pass, a skipped one, a pass whose commit left a candidate unfiled) to `backoff_max`, and is at least a
   `RateLimited` pass's `Retry-After`; it is the channel's alone, so a channel whose passes always fail or are
   skipped (no row, a thread Slack will not serve) delays none other. **The commit** is one rule, made once every queued or in-flight record of the channel at or below
   the limit, the pass's candidates among them, has returned (a candidate already filed counts as filed; one queued or in flight counts only
   when its filing returns `Filed`, and the boundary never reaches a `ts` still pending, which the recheck at issue would
   drop): the limit is the pass's `horizon`, or for a `Truncated` pass the lesser of `horizon` and its last candidate,
   since it handed over only its oldest; then, among the candidates at or below the limit, all filed moves the boundary to the limit, the oldest not filed leaves it where it was,
   and otherwise it moves to the last one filed before the first that was not. A candidate above the limit is not judged.
   The commit is `Action::Commit { channel, boundary }`: `State::commit` has moved the in-memory boundary, which drops
   the set's filings at or below it, and the owner writes the boundary durably (`state::write`); nothing comes back, since
   a stale file only costs reads. A `Truncated` pass owes the channel another at once; among ready channels the least recently passed goes first, so a
   channel truncated at every pass does not hold another's owed pass. Limits: an absent recipient
   refuses each queued filing in turn, one per `FILE_GAP`, up to 512 records in about 100 s, each owing the same pass,
   and since the one queue is first in, first out, every other channel's filings wait behind that backlog for the same
   time (a pass's 200 candidates take about 40 s); a record that is in the queue when the start's read settles it is
   dropped at issue and owes nothing; a live record that no pass found (a reply under a parent
   Slack's plan hides, which guarantee 26 says recovery does not find, or a post Slack was not yet showing to the pass,
   not observed in the measurements guarantee 26 cites) is filed only by its own event: if that filing is refused, or
   the event arrives after a commit has passed its `ts` (it is then settled and queues nothing), nothing finds it, since
   a refused filing is not retried and no later pass reads it; instants are `std::time::Instant`, and a test supplies
   offsets from one start.
28. **The setup's checks verify the tokens before anything is saved, under the rules every call keeps.**
   `api::identify(http, token)` (the bot's, and the user token's too: `auth.test` answers a user token's `team_id` and `user_id` and no `bot_id`) is one `auth.test` with no expected workspace, under the read rules (guarantees 5 to 10
   and 13 to 15; its pacing instant is its own, so a rate limit answers `Limited` and refuses nothing after it); it
   answers the token's `team_id` as `Identified.workspace`, its `user_id` and `bot_id` as `Identified.own`, and its `team`
   as `Identified.name` (the workspace's name: control characters dropped and trimmed, and none when absent, not a
   string, empty or past `MAX_NAME`; words only; `api_tests::the_workspaces_name_comes_from_auth_test_and_a_bad_one_is_none`); a reply
   without a valid `team_id` or `user_id` is `Malformed`, Slack's refusal is `Slack(code)`, and `bot_id` is kept only
   when valid. The service's `Api::auth_test` keeps its workspace check (`WrongWorkspace`), and both read the answer
   through the same two private functions. `socket::check_app_token(http, app)` is one `apps.connections.open`, built by
   the private `connection_url` that the listener's open also calls (one request, two callers); the URL it answers
   must pass guarantee 20's predicate with scheme `wss` (`Http::check_websocket`, the first step of `Http::websocket`)
   and is then dropped, never dialed. Its codes are the socket's (`rejected` only for 401, 403 or `ok: false` with a code of `api::TOKEN_REFUSALS`, the
   codes that name the token, or `missing_scope`; `unavailable` for any other status or code, Slack's own state;
   `malformed`, `Http(code)` with `destination` for a foreign URL); no error carries the token, the URL or the ticket. Limits: an undialed URL is
   assumed to cost nothing at Slack (documented, not measured: the URL is one-use and expires); an app token of another
   app than the bot token's is not detected here, since the check never dials (a dialed socket would be one of that
   app's connections and take its events); the service's listener detects it at its first `hello` or envelope naming
   its app and ends `wrong_app` (guarantee 17). Their caller is the daemon's
   `chat.connect` (`rust/backend/src/chat/setup.rs`, through its `Verifier`); `identify_for_test` and
   `check_app_token_for_test` take the fake's base and exist only under `cfg(test)` or `test-support`, as
   `Service::start_at` does.
29. **The picker's three calls are bounded, whole, and send a join once.** `Api::channels` is `conversations.list` with
   `types=public_channel,private_channel`, `exclude_archived=true` and `limit=200`, following Slack's cursor inside the
   one call: at most five pages (wire's `MAX_LISTED`, 1,000 channels, held equal to the page size times the pages by a const assertion), each page one read under guarantees 13 to 15 (its own
   deadline, the retry header, the body bound), a page of more than 200 entries or with no `channels` array `Malformed`,
   a cursor over `MAX_CURSOR` (1 KiB) `TooLarge`, the cursor just sent coming back `Repeated`; a failure on any page is
   the call's, so the list is whole or an error, and `more` is true when a cursor remained after the fifth page. An entry
   with no valid channel id (`C…` or `G…`; a direct message or a bare token is skipped), or with no name left after
   `clean` at `MAX_NAME`, is skipped. Each entry is a `Channel`: `private` is true unless Slack says `is_private` is
   `false`, and `member` false unless it says `is_member` is `true`, so a reply that does not say never offers more. The
   private channels listed are those the bot is in (Slack's rule as its documentation gives it; not yet seen live, so the
   fallback holds: an absent `is_member` reads false). `Api::channel(id)` is one `conversations.info` answering the same
   `Channel` (`Malformed` for no name; an id that is no channel's is `BadRequest`, unsent), and it is the one function that
   reads `conversations.info`: `channel_name`, guarantee 16's rule, reads through it (a channel is named when it is not
   private or the caller maps it). `Api::join(id)` sends one `conversations.join` (POST, bearer, 30 s as a post, never
   resent, no cancellation repeats it): an `ok`, or the code `already_in_channel`, is `Joined`; Slack's definite refusal
   (`missing_scope`, `method_not_supported_for_channel_type`, `channel_not_found`, `is_archived`, any other
   non-transient code, a non-5xx status as `http_<code>`, or a request that never left) is `Refused(code)`; anything
   without a valid acknowledgement, a rate limit included, is `Unknown`, which the caller settles by reading
   `Api::channel`'s `member`. A join and a post share one reading of an answer (`acknowledgement`). Limits: the list shows
   a private channel only after the app is invited (a person's step); a workspace of over 1,000 channels lists the first
   1,000 Slack returns and says `more`; the join's success path needs the `channels:join` scope, seen only in a person's
   use of an app made from the setup manifest, so the tests script its answers.
30. **A window's pairing is admitted, joined, edited, written and reloaded under one lock, and refused the window's way.**
   The daemon's `chat.channels` and `chat.pair` (`rust/backend/src/chat/setup.rs`, each a job) take a window's connection
   only (`bad_request` from any other, before the payload is read), so no row's agent changes the project's chat.
   `chat.channels {workspace?}` lists the request's workspace, absent `chat.toml`'s (C4, fixed whatever runs, so an older
   window lists the default's as before), and needs that workspace's running service (`unavailable`), and answers `Api::channels` (guarantee 29) as the wire's list:
   the id, the cleaned name, `private` and `member`, `more` as the call says; a failure is `code_of`'s code and the list
   is never partial. `chat.pair {row, channel?, workspace?}` pairs in the request's workspace, absent `chat.toml`'s, and runs in this order: the row is admitted as an action is, against that workspace's running service, with no mapping
   needed since it may map to nothing yet (`Chat::admit_row`: `unavailable`, `stale_row`, the inert anchor `unmapped`);
   for a channel, that workspace's `Api::channel` gives its facts (one read; Slack's `channel_not_found` is `no_such_channel`, PS9: the channel is another workspace's, `chat_setup_tests::a_channel_of_another_workspace_is_no_such_channel`), a member passes, a private channel the app is not in is
   `not_in_channel` unjoined, a public one is joined once (`Api::join`: a refusal, whose Slack code is logged, is
   `not_in_channel`; no acknowledgement is settled by one more read of the channel and never by a second join); then
   the setup lock is taken (`Chat::setup`), after the Slack calls, so a slow call holds no other window's setup, and
   under it, on the blocking pool, the hub fact is asked again (`unavailable`, so a hub deposed since its boot writes
   nothing in the minute before it steps down), `chat.toml` is read through `config::read_file` (the reload's own read,
   not the boot's injected `Reader`, which stays the boot's alone: the text edited is the text the reload will load, a
   hand edit made since the start included), and `config::pair` edits it with the clock's now as the floor of a new
   mapping in a delivering file. `channel_taken` is the edit's refusal; `not_saved` is a text the load refuses (never
   rewritten), a missing file, a missing config folder or a failed write, each logged with a code or an error kind and
   no text; an edit that changes nothing writes nothing and does not reload; a changed file is written whole through
   `config::write_config` and reloaded under the same guard, and the reply is the reload's, after its `SETUP_WAIT`
   (a reload that ended in another state than running is still a saved pairing: the window resolves again). Limits: the
   row's folder is the one the admission read, so a row removed while the pairing waits for the lock still gets its
   folder's mapping (a mapping needs no row); a join is a Slack side effect that stays when a later step refuses; the
   reload costs every watching window a resync (guarantee 21). Tests, in `rust/backend/src/server/chat_setup_tests.rs`:
   see the table. `chat.connect` is guarantee 31.
31. **A window's connect checks the three tokens before anything is written, saves them owner-only and reloads, and no token
   byte leaves the secrets file and its owner-only temporary file.** The daemon's `chat.connect` (`rust/backend/src/chat/setup.rs`, a job). A window's alone: any other connection is `bad_request` before anything is read (M3 for the setup ops; the role is the client's own declaration, as D13's is). Then, in order: a chat that never registered (not the hub at its boot) or whose retirement began, and a hub the topology no longer names (`is_hub`, asked at the op on the blocking pool), are `unavailable`; a request with no `user_token` (an older window's) is `bad_request` before anything else is checked; each token in the load's own grammar (`config::bot_token`, `config::user_token`, `config::app_token`, each a `Secret` from there on), else `bot_token_refused`, `user_token_refused` or `app_token_refused`; the bot token's `identify` (guarantee 28), the user token's `identify` (its `Slack(code)` of `api::TOKEN_REFUSALS`, or a `team_id` other than the bot token's, is `user_token_refused`; Slack's state or the transport is `failed`), then the app token's `check_app_token`. Only Slack's answers that name the token refuse it: `identify`'s `Slack(code)` with a code of `api::TOKEN_REFUSALS` is `bot_token_refused`, `check_app_token`'s `Rejected` (401, 403, or a code of `TOKEN_REFUSALS` or `missing_scope`) is `app_token_refused`; Slack's own state (a 5xx, a 429, `ratelimited`, `internal_error`, `service_unavailable`, any other code) and a transport failure are `failed`, logged by the fixed code, so a person with good tokens is never told during an outage that Slack refused them. Only then, under the setup lock (`Chat::setup`), the closed mark is read again, so a retirement that began while Slack was asked writes nothing (`unavailable`); then `chat.toml` is read as it is now through `Chat::load`, the one load (on the blocking pool; a topology that no longer names this computer is `unavailable` there too): one the load refuses is `not_saved` and is never rewritten, a missing one is to be written fresh (`config::fresh`, the connecting workspace then the file's); a workspace with no secrets file yet is `too_many_workspaces`, nothing written, when the folder already lists `MAX_WORKSPACES - 1` named files beside the default's, admitted, refused or past the bound alike (`Secrets::named`: a new name sorting before a served one would push that one past the load's bound; `a_connect_that_would_push_a_served_workspace_past_the_bound_is_refused`); the secrets text (`config::secrets`, the four keys `workspace`, `bot_token`, `user_token` and `app_token`) through `write_secrets` with `Writers::platform()` (owner-only from its creation) to the workspace's one file, `Secrets::file_of` against `chat.toml`'s workspace (`chat-secrets.toml` for it, `chat-secrets-<id>.toml` for any other: a connect adds a workspace or gives one new tokens, and never touches another's file; `a_connect_of_another_workspace_adds_it`), then the fresh `chat.toml` through `write_config`, a failed write `not_saved`; then `Chat::reload` under the same guard, the bot token's name kept (`Chat.names`), and the reply `{workspace, name}` (the name as `auth.test` gave it, absent when it gave none). `other_workspace` is no longer sent (an older daemon's code). Nothing is written before all three tokens pass, and an admitted `chat.toml` is left byte for byte. The writes on the blocking pool log nothing: they answer their reason (the load's refusal or the error's kind, never a value of the files) and the connect's task logs it, so a test's capture of every thread of its runtime sees each line the connect makes. The checks reach Slack through the `Verifier` `Chat::boot` takes beside the starter (`Verifier::product`: one client, made at the first check, so every connect's calls share its bound of `MAX_OPERATIONS`; a test's reaches the fake Slack), so no test reaches Slack. No log line, reply or error carries a byte of a token, and the request payload is never logged: a refusal is the fixed code and text. Tests, in `rust/backend/src/server/chat_setup_tests.rs`: see the table; `logged` runs a test on a runtime whose every thread, the blocking pool's included, logs into one capture. Limits: the reply means the tokens were saved and the reload's wait ended, not that the chat runs: a start that failed still replies `{workspace}` and `chat.resolve` then says `unconfigured` (`start_failed`), and a start still waiting (held by another daemon past `SETUP_WAIT`) replies after that wait and resolves by the file; the window resolves again after every reply. A connect whose window closes runs to its end (the job outlives its connection); its reply can take three Slack checks and a reload (`SETUP_WAIT` at most for each wait). An app token of another Slack app than the bot token's passes both checks (guarantee 28's limit) and replaces working secrets, with no way back but another connect. Two daemons on a shared home with no `hosts.toml` each count as the hub and their edits are not serialized across computers (decision D1).

32. **A file is shared once, and only its share can post.** `Api::post_file(transfer, channel, thread, kind, name,
   file_name, bytes, comment)` makes Slack's three upload steps under one of `MAX_TRANSFERS` (2) transfer permits (a `Transfers`, which clones share: the daemon gives one to every
   workspace's service, C9, so it holds at most two files whatever the number of workspaces, and one workspace's two long
   transfers make another's `busy`, each bounded by its deadline; `service_tests::two_services_share_the_transfer_permits_they_are_given`), which
   the caller takes first with `Api::transfer` (`try_acquire_owned`, so a third at once is `Err(Http(Busy))` at once and
   a closed client's is `Err(Closed)`) before it reads the file, so no file is read into memory without a permit; the
   call holds it across all three steps, and it is freed on every path, a caller that drops the call included.
   A window asks at most one thumbnail at a time per host (fe-ui's pictures owner), so one window loading pictures leaves
   the other permit for a row's file post; two windows loading pictures from one hub at once can hold both, and a row's
   post is then `busy` (exit 2).
   Input it cannot carry is `Err` unsent: a closed client, a channel that is no id, a thread that is no `Ts`, a post's
   author name (`clean`ed, within `MAX_NAME`), a file name that is empty, over `MAX_FILENAME` (256 bytes) or holds a
   control character, `/` or `\`, bytes empty or over `MAX_CHAT_FILE` (16 MiB, wire's, re-exported), an empty comment, or a label and comment
   escaped past `MAX_TEXT`. Step 1 is a read (`files.getUploadURLExternal` with `filename` and `length`, guarantees 13 to
   15: a read's deadline, a retry header in force refusing it `Limited` unsent, a 429 or `ratelimited` setting the wait);
   its answer's `file_id` must be an id and its `upload_url` at most 2 KiB, else `Refused("malformed")`. Step 2 posts the
   bytes (`application/octet-stream`, `TRANSFER_DEADLINE`, 60 s) to that URL with no authorization (guarantee 5 refuses a
   host off Slack, `destination`, before a connection). Any failure at step 1 or 2, a timeout included, is
   `Refused(code)` (the HTTP core's code, Slack's, `http_<status>` or the call's own) and step 3 is never sent, so
   nothing is shared; the client closed between the upload and the share is `Refused("closed")`. Step 3,
   `files.completeUploadExternal` (JSON, the bearer, `POST_DEADLINE`), is the share, the only mutation: `files` (the
   file id and its name as `title`), `channel_id`, `thread_ts` when given, and `initial_comment`, the label `<name>
   (<kind> via Ship of Tools)`, then `: ` and the comment when there is one, made Slack's by the one private escape
   (guarantee 12), the bound on the escaped whole; a file share takes no custom author or metadata. A person's share makes
   steps 1 and 3 under the person's own user token (guarantee 12's), never the bot's: with none held it is
   `Err(NoPersonToken)` unsent, its `initial_comment` is the escaped comment alone and absent without one, `name` is
   neither checked nor sent, and a refusal of either step whose code names the token (`refuses_the_token`) forgets it
   before the answer, as a text post's does (unmeasured: `files:write` on a user token, the upload and share made under
   it). Its answer is read by
   the one `acknowledgement` a post's is: `ok` with an id at `files[0].id` is `Posted { file, ts }`, `ts` the share's in
   the channel (`shares.public` or `shares.private`) when Slack names it, else `None`; `ok` without the id is `Unknown`;
   Slack's definite refusal is `Refused`, a 429 `Limited`, anything else `Unknown`; nothing resends. The upload URL and
   the file id are held inside the call and never formatted; `FileOutcome` carries fixed codes, the file id and the
   `ts`. Limits: Slack keeps a PNG's pixels and not its text chunks, so the bytes read back are not the bytes sent; the
   bytes are held twice while step 2 runs (the caller's and `Http`'s clone for its one hop), so two callers waiting hold
   at most 64 MiB of files; a caller that drops the call frees its permit while `Http` runs the step it had sent to that
   step's deadline (guarantee 7), so what is held is bounded by `Http`'s sixteen operations, not by the two permits,
   and the daemon never drops a file call while its connection ends. Unmeasured until a person's first use (each built to fail closed, pinned against the
   fake): that the upload URL is on a Slack host, that `thread_ts` shares into the thread (else the file lands at the
   top level and still reads `Posted`), and where step 3's answer puts the share's `ts`. Tests: see the table.


33. **A picture is fetched only from a channel it is shared in, bounded before and while it streams, and only a body
   served as one of four picture types is returned.** `Api::fetch_file(channel, file, variant)` (`FileVariant::Thumb` or `Original`) is a read under one of
   the same `MAX_TRANSFERS` permits (guarantee 32), taken through `Api::transfer` before anything is sent, so a third
   transfer at once, an upload included, is `Err(Http(Busy))` at once, and the permit is freed on every path. A closed
   client, a channel or file that is no id, and (after one `files.info`, a read under guarantees 13 to 15) a file that is
   not shared in `channel` (the channel is one of its `channels` or `groups`, or a key of `shares.public` or
   `shares.private`) are `Err` with nothing downloaded; an `info` that is not the file asked for is `Malformed`. A file
   whose `mimetype` is not one of wire's `PICTURE_MIMES` (`image/png`, `image/jpeg`, `image/gif`, `image/webp`) is `Ok(NotAnImage)`, unfetched. A
   thumbnail is the first of `thumb_360`, `thumb_480` and `thumb_160` that Slack names, whatever the size of the
   original; with none, the original stands in only when `files.info`'s `size` is at most `MAX_THUMB` (256 KiB), else
   `Ok(NoThumbnail)` unfetched, which is not final (Slack can make a thumbnail after an upload), where `TooLarge` is. An original (`url_private_download`, else `url_private`) is fetched only when its `size`
   is at most `MAX_CHAT_FILE` (16 MiB), else `Ok(TooLarge)` unfetched; a missing `size` is `Malformed`. A URL is at
   most 2 KiB. The one GET carries the bearer to a Slack host (guarantee 5), under the variant's deadline (a
   thumbnail's is a read's 15 s, an original's `TRANSFER_DEADLINE`) and the variant's bound counted as the body
   streams (guarantee 8), whatever `size` claimed: one byte over is `Ok(TooLarge)`. A 429 sets the reads' wait as any
   read's does, so a throttled files host also holds every read, and is `Err(Limited)`; another status is
   `Err(Status)`; an answer whose `Content-Type` (parameters dropped, lower case) is not one of the four `mimetype`s is
   `Ok(NotAnImage)`, so Slack's sign-in page and an SVG are never a picture; the type is Slack's word and the bytes are
   not looked at (the window's bounded decode is their gate); a picture with no bytes is `Malformed`. The call
   rechecks that the client is open after `files.info` and before the GET, so a retired service sends nothing more.
   The result is `Image { mime, bytes }`, `mime` the served type; its `Debug` prints the byte count, not the bytes. The file's URLs are held inside the call and never formatted; `ApiError` is a fixed code. Limits: a
   fetch is never resent by the call; while its caller waits it holds the answer's body and a copy (at most 32 MiB for
   an original, 64 MiB with two permits), and a call dropped before its GET ends leaves that GET to `Http` (guarantee 7),
   at most sixteen at once, each to its bound and deadline, so the daemon never drops a fetch while its connection
   ends; `files.info` listing a bot's channels, `thumb_*` for every image type and the
   served `Content-Type` are unmeasured until a person's first use (each built to fail closed against the fake: a fetch
   that finds no channel is `BadRequest`, no thumbnail falls back to a small original or `NoThumbnail`). Tests: see the
   table.

The timestamp rule (exactly `<seconds>.<six digits>`) is wire's `Ts`; this crate builds only `Ts` values, so a record
never holds another form.

## Owns
`normalize.rs`: `normalize`, `Context`, `Normalized`, `Unrepresentable`, `ts_key`; it re-exports wire's record bounds (`MAX_RECORD_BYTES`, `MAX_TEXT`, `MAX_FILES`, `MAX_ID`, `MAX_NAME`, `MAX_FILENAME`, `MAX_MIME`) and the id grammar `is_id` (as `valid_id`), which wire owns. `http.rs`: `Http`,
`Request`, `Response`, `Secret`, `HttpError`, `is_slack_host` and the bound constants (`MAX_OPERATIONS`, `MAX_BODY`,
`MAX_SCAN_BODY`, `MAX_REDIRECTS`). `api.rs`: `Api`, `HistoryQuery`, `ScanQuery`, `Bounds`, `Upper`, `Continuation`,
`Page`, `ScanPage`, `MAX_SCAN`, `PostKind`, `PostOutcome`, `FileOutcome`, `Transfer`, `Transfers`, `Api::with_person`, `ApiError::NoPersonToken`, `FileVariant`, `FileFetch`, `MAX_TRANSFERS`, `Channel`, `ChannelList`, `JoinOutcome`, `Identity`, `identify`, `Identified` (and, under `cfg(test)` or `test-support`, `identify_for_test`), `ApiError` and the deadline constants and `MAX_NAMES`, `ASSUMED_RETRY`; `MAX_PAGE`, `MAX_CURSOR`, `MAX_LISTED`, `MAX_CHAT_FILE` (the largest file a post or a fetched original carries, 16 MiB), `MAX_THUMB` (the largest thumbnail, 256 KiB) and `MAX_RETRY` (wire's `MAX_RETRY_SECS`) are wire's, re-exported. `socket.rs`: `Socket`,
`Gap`, `GapCause`, `Taken`, `Status`, `SocketError`, `check_app_token` (and, under `cfg(test)` or `test-support`, `check_app_token_for_test`) and the bound constants (`MAX_PENDING`, `MAX_PENDING_BYTES`, `MAX_MESSAGE`);
`http.rs` also holds `Http::websocket`, the one way to a websocket, and `Http::check_websocket`, its destination check alone. `service.rs`: `Service`, `Status`, `ServiceError`, the end codes (`WRONG_APP`, `APP_TOKEN_REFUSED`, `BOT_TOKEN_REFUSED`) and the crate-private `End`. `watch.rs`: `Watches`, `State`, `Loaded`, `Wake`, `WatchError` and the bound constants (`MAX_CHANNELS`, which wire's `MAX_WATCHED` repeats and `chat_and_wire_agree_on_the_numbers_they_share` holds equal, `MAX_KEPT`, `MAX_OVERLAY`). `mail.rs`: `SENDER`, `MAX_MAIL`, `Filed`, `is_mail`, `body`, `filed_of`. `state.rs`: `State`, `StateError`, `MAX_FILED`, `encode`, `decode`, `path`, `write`. `traverse.rs`: `Order`, `Passed`, `Outcome`, `pass`, `MARGIN`, `MAX_PASS_READS`, `MAX_PASS_MAIL`. `delivery.rs`: `Delivery`, `Action`, `Filing`, `Verdict`, `Timing`, `FILE_GAP`, `MAX_QUEUED`. `config.rs`: `Config`, `Credentials`, `Secrets`, `SecretsFile`, `SECRETS_FILE`, `Resolution`, `ConfigError`, `resolve`, `MAX_WORKSPACES` (wire's, re-exported), the token grammar for a pasted token (`bot_token`, `app_token`, `user_token`), the writes (`fresh`, `secrets`, `pair`, `PairError`, `Write`, `Writers`, `write_config`, `write_secrets`), `read_file`, `MAX_FILE`, `config_dir`, and the crate-private `is_slack_id`, the one grammar of a Slack id. `fake_slack/` (under `cfg(test)` or `test-support`): the scripted fake Slack,
its websocket peer (`ws.rs`), a channel that answers history as measured (`history.rs`, which states what it infers) and
Slack's three upload steps (`upload.rs`, `Upload`) and one file's `files.info` and URLs (`files.rs`, `Files`), and
several workspaces in one fake (`teams.rs`, `Teams`).

## Calls in
`sot_protocol::chat::{ChatRecord, ChatAuthor, ChatFile, Origin, Ts, is_id, escape_text, post_text_fits, MAX_*}` (and `text_segments`, `TextSegment`; `page_reply::unsafe_char`), `sot_protocol::ops::{MAX_PAGE, MAX_CURSOR, MAX_LISTED, MAX_RETRY_SECS, MAX_WORKSPACES}` (and, in a test, `MAX_WATCHED`), `serde_json::Value`; reqwest, rustls and
webpki-roots and tokio-tungstenite inside `http.rs` (`socket.rs` uses the library's `Message` and `Error` types; `api.rs` uses
`reqwest::Url` to build query strings). Dev only: `sot_platform::test_isolated` (`sot-platform` with `test-support`).

## Does not
Open a socket except through `Http::websocket`, call a file method outside `config.rs` and `state.rs`, write a credential anywhere but the secrets file `Secrets::file_of` names, through platform's private write, read a token from the environment, file mail, write a boundary (`delivery` decides each commit and the caller writes it), or resolve a row (it maps folders to channels from supplied facts). Define the
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
request (with its path, query parameters, `Authorization` header and raw or JSON body), connection and handshake failure.
`Teams` answers several workspaces at once by the request's bearer, the fixture of every two-workspace test: each
workspace's own bot, user and app tokens (built at run time from its id), its channels, `auth.test` naming its id and
name, `bots.info` its app, `apps.connections.open` a URL whose ticket picks that workspace's socket, whose hello and
envelopes name its app, and `revoke`, from which its bot token is refused `token_revoked`.
`Upload` answers the three file upload steps as SlackClaw measured them, an upload URL on `files.slack.com` (or a host a
test names) at the fixture's port, and a share with or without its `ts`. `Files` (a struct of public fields a test changes with struct update) answers one file's `files.info` (its
channels as `shares` or as the older `channels` and `groups`, its `thumb_*` and original URLs on `files.slack.com`) and
those URLs' bytes with a `Content-Type` a test sets, or a redirect; that a bot token's `files.info` lists the channels is inferred, not measured. It flushes after every
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
| 2 | own posts by supplied identity only | `normalize_tests.rs`: `a_post_made_with_a_persons_token_reads_as_theirs` (pins the existing rule, green before the person's post changed), `normalize_members_bots_edits_deletes_and_file_stubs` (member prefix, member username, other bot), `with_no_identity_supplied_a_member_with_a_forged_prefix_is_a_member`, `a_prefix_in_an_own_posts_text_is_only_text`, `an_unreadable_own_post_keeps_its_origin_and_an_unreadable_reply_its_thread`, `an_own_file_share_takes_its_origin_from_its_label_and_a_members_does_not` (a row's label, a person's read as no label, an escaped name, a label with no comment, a comment holding a marker, five texts that are no label as sent, a member's and another bot's labeled share, metadata first, an own text post's label left to its metadata, an edit); `api_tests.rs`: `a_file_share_read_back_through_history_keeps_its_origin` (`post_file`'s shares answered by the fake's history) |
| 3 | bounds | `normalize_tests.rs`: `text_is_cut_on_a_character_boundary_and_marked`, `names_filenames_and_mime_are_cut_and_marked`, `only_eight_files_are_summarized_and_the_rest_counted`, `an_id_over_64_bytes_or_not_ascii_is_not_carried`, `a_record_never_exceeds_32_kib_of_json`, `names_lose_control_characters`, `a_normalized_record_round_trips_through_the_wire`, `a_filled_record_stays_within_the_record_bound` |
| 4 | attachment only | `normalize_tests.rs`: `an_attachment_only_message_keeps_its_files_and_has_no_text` |
| 5 | Slack destinations and authorization | `http_tests.rs`: `private_redirect_keeps_slack_auth_and_refuses_foreign_destinations` (the bot's token and a person's user token), `slack_hosts_are_slack_com_and_its_dot_boundary_subdomains`, `a_mutation_is_sent_once_and_never_followed_to_a_redirect`, `an_ambient_proxy_variable_is_ignored` (the test re-runs itself through `sot_platform::test_isolated` as a child with every proxy variable naming a dead address and expects a direct request; the helper fails the parent when the body was not entered exactly once or exited non-zero), `the_product_client_builds_and_refuses_off_port_plain_and_ip_destinations` (no network) |
| 6 | verified server, and no test root in a shipped binary | `scripts/tests/release-features-check.sh --prove` (the gate runs it: green for the real tree and for the feature on a dev-dependency alone; red with the feature on a normal dependency, in `default`, on the frontend, in a target table, or on a member only `--workspace` builds); the compile error is shown by a release build of `sot-chat` with the feature failing and a test build passing, and `sot-backend`'s `the_exported_fake_slack_answers_one_scripted_reply` (the export works); `http_tests.rs`: `an_untrusted_certificate_fails_before_any_http`, `a_name_the_certificate_does_not_cover_fails_before_any_http` (each with a passing control; the second also the resolver's refusal) |
| 7, 8 | deadline, admission, body bound | `http_tests.rs`: `chat_operations_keep_bounds_after_caller_drop`, `a_requests_own_body_bound_replaces_the_default_and_still_counts_the_stream` |
| 9 | one send, one request | `http_tests.rs`: `one_send_is_one_request_even_when_the_server_closes_mid_response` |
| 10 | no secret in a formatted value | `http_tests.rs`: `chat_secrets_stay_inside_daemon_boundaries` |
| 11 | exact history pages | `api_tests.rs`: `history_pages_exact_query_without_silent_completion`, `a_bound_without_exactly_six_fraction_digits_is_refused_before_sending`, `a_thread_continuation_under_a_lower_bound_alone_is_refused_unsent`, `a_scan_reads_999_and_its_bound_is_its_own`, `a_scan_page_names_its_raw_extremes_and_every_parents_latest_reply`, `a_limited_page_is_a_page_with_its_flag`, `a_replies_scan_at_999_may_hold_1000_with_its_parent`, `a_replies_page_holding_only_its_parent_with_no_more_is_an_end`, `a_scan_rechecks_its_bounds_and_refuses_a_limit_over_999` |
| 12 | one post, origin kept; a person's own token, nothing of the app | `api_tests.rs`: `a_persons_post_goes_with_their_token_and_names_nothing_of_the_app`, `with_no_person_token_a_persons_post_is_refused_unsent_and_a_refused_token_is_forgotten`, `lost_post_answer_is_unknown_and_never_resent`, `a_post_carries_its_author_and_every_unconfirmed_answer_is_unknown`, `a_post_read_back_through_history_keeps_its_origin`, `a_message_text_is_sent_escaped_and_bounded_escaped` |
| 13 | retry header | `api_tests.rs`: `retry_header_paces_reads_and_preserves_mutation_draft` |
| 14 | deadlines | `api_tests.rs`: `every_operation_ends_at_its_deadline`, `name_lookups_together_end_at_the_names_deadline` |
| 15 | no raw Slack text | `api_tests.rs`: `slack_text_and_identity_failures_reach_no_caller_raw` |
| 16 | names | `api_tests.rs`: `names_fall_back_to_ids_without_unbounded_cache`, `fill_names_resolves_mentions_within_the_deadline_and_the_cache`, `a_cached_channel_name_is_read_with_no_request` (`Api::cached_name`, which the daemon's `chat.resolve` reply reads for a mapped channel's `name`: the cache alone, never a request), `the_configured_channels_names_are_looked_up_once` and `the_lookup_stops_at_a_scope_error_and_at_the_deadline` (`Api::channel_names`), `a_mapped_private_channel_is_named_and_an_unmapped_one_is_not` (the one rule: mapped, public or private; a mention of an unmapped private channel gets none); the daemon's `chat_watch_tests.rs`: `a_change_event_carries_the_names_the_cache_holds`; `chat_ops_tests.rs`: `a_resolve_carries_the_name_the_start_found_and_none_before_it` (the owner paragraph's lookup: a resolve never waits for it), `a_resolve_carries_a_mapped_private_channels_name` |
| 17 | ack first (C11) | `socket_tests.rs`: `socket_ack_precedes_slow_api_and_dispatch` (the fake sends each envelope only after the previous one's acknowledgement while the dispatcher is stalled on a lookup that never answers), `a_write_that_cannot_finish_retires_the_socket_within_the_write_bound` (a peer that never reads fills the buffers; the socket is dropped, not held, with idle far past the test), `an_envelope_of_another_app_is_not_acknowledged_and_ends_the_listener` (envelopes naming no app and the bot's own acknowledged, another app's not, the socket closed, one socket accepted and one open asked, `wrong_app`; a bot whose app is unknown acknowledges all), `a_hello_naming_another_app_ends_the_listener_before_any_envelope` (never live, nothing acknowledged; a hello naming no app, an empty one or a number is live) |
| 18 | one socket (C12) | `socket_tests.rs`: `disconnect_refresh_and_silent_close_keep_one_socket` (warning, refresh, silent close, a peer that ignores close, endless refreshes; peak open 1; stop bounded and final), `a_peer_that_ignores_close_is_forced_down_within_the_bound`, `the_product_numbers_are_the_measured_ones` (all eight product timings), `a_silent_socket_is_replaced_after_the_idle_bound` (ping shorter than idle, as in the product; replaced no sooner than idle and within a stated bound), `a_link_that_answers_is_kept_past_the_idle_bound`, `a_chatty_peer_that_never_answers_keeps_its_link`, `a_peer_that_only_pings_keeps_its_link` (a peer ping alone resets the idle clock), `every_slack_ping_is_answered_with_its_payload_while_the_consumer_stalls_and_the_queue_is_full` (a pong frame with the payload, in both states), `a_bare_end_of_stream_is_replaced_within_the_pacing_and_latches_a_gap`, `reconnects_are_paced_after_a_refusal_and_after_a_hello`, `stop_during_the_reconnect_pause_does_not_wait_for_it` (every test here that holds a link or a measured duration against a bound runs under `steady`: an attempt that fails beside a ticker that saw a pause of that bound's margin is run again, up to five times; a failure on an unpaused host fails the test and names the longest tick) |
| 19 | gaps (C13) | `socket_tests.rs`: `a_gap_opened_by_an_outage_survives_a_reconciliation_made_during_it`, `a_failed_event_or_a_lost_socket_during_a_reconciliation_keeps_the_gap`, `a_reconciliation_begun_before_a_hello_is_refused_by_it`, `overflow_latches_gap_without_blocking_ack` (a failed event; 300 envelopes over the count with every one acknowledged; a loss during a reconciliation survives it; ten 900,000-byte events over the byte budget), `an_events_message_keeps_its_origin_through_the_socket` |
| 20 | websocket bounds and destinations | `socket_tests.rs`: `a_message_over_one_mib_is_refused_before_it_is_read_whole` (one byte over is not delivered, exactly the bound is), `a_foreign_websocket_url_fails_visibly_and_leaks_nothing` (also eight spellings refused by `Http::websocket`), `a_rejected_open_ends_the_listener_and_an_unavailable_one_is_retried` (`invalid_auth` and `missing_scope` at 200, `token_revoked` at 403 and `not_authed` at 401 each one open and `app_token_refused`; a bare 403, a bare 401 and `internal_error` retried), `an_envelope_of_any_type_naming_another_app_ends_the_listener` (`slash_commands` and `interactive`) |
| 21 | configuration (M1, M2, C14's load half) | `config_tests.rs`: `a_user_token_is_optional_of_its_own_form_and_formats_as_a_mask`, `mapping_refuses_aliases_and_duplicates`, `a_mapping_whose_folder_does_not_exist_is_ignored_and_keeps_its_channel`, `resolve_explains_unmapped_invalid_and_disabled_delivery`, `a_second_workspace_as_the_files_own_is_refused_at_load`, `a_mapping_names_its_own_workspace_and_without_the_key_is_the_files`, `a_channel_id_is_mapped_once_whatever_its_workspace`, `pairing_in_another_workspace_writes_its_workspace_key_and_in_the_files_none`, `every_refusal_has_an_input_a_code_a_place_and_its_own_explanation` (a row per `ConfigError` variant, the boundaries, the two file names), `a_token_prints_no_bytes_and_a_load_error_quotes_no_secret_value`, `a_secrets_refusal_names_the_key_the_file_must_not_hold_and_never_a_value`, `the_reference_samples_are_accepted_by_the_parsers`, `loading_the_configuration_opens_no_secret_file` (an observing reader), `the_real_read_takes_a_regular_file_up_to_the_cap_and_nothing_else`, `a_planted_fifo_or_device_is_refused_without_a_wait` (the read runs on a thread with a 10 s guard that only a failing read reaches), `the_files_are_named_in_platforms_config_folder`, `a_delivering_file_has_at_most_32_mappings_and_a_file_not_delivering_has_no_bound`, `a_delivering_file_refuses_a_mapping_without_its_own_floor`, `a_mappings_own_mail_after_raises_only_its_channels_floor` (numeric order, no floor without the hub's value, the resolution's floor); the writes: `a_fresh_file_and_a_secrets_text_parse_back_and_format_no_token`, `pairing_edits_one_mapping_keeps_what_it_ignores_and_returns_only_what_loads` (the toml writer's text loads back; the read's cap), `a_refused_file_is_never_rewritten`, `a_delivering_files_new_mapping_carries_the_floor_it_is_given`, `the_secrets_go_through_the_private_writer_alone` (an observing writer pair, then platform's: the folder made, the secrets 0600 on Unix); the secrets files: `the_secrets_files_are_listed_by_their_two_names_alone` (decoy names, a subfolder), `each_secrets_file_is_admitted_or_refused_alone` (a dangling link on Unix is `Unreadable`), `a_named_file_must_hold_its_workspace_and_the_defaults_is_chat_secrets`, `a_folder_of_many_secrets_files_opens_at_most_the_bound` (refused files counted), `each_workspace_has_one_file_a_connect_writes`, `a_secrets_file_has_one_form`; `chat_tests.rs`: `every_connected_workspace_starts_its_own_listener` (the no-user-token line per file), `a_named_files_refusal_is_not_the_defaults`; `server/chat_ops_tests.rs`: the file post's refusals include a named secrets file at the config folder's top |
| 21 (the caller, M3) | every chat action rechecks the row and the expected pair; `resolve` joins the status | `rust/backend/src/chat_tests.rs`: `stale_row_or_mapping_cannot_post_or_fetch`, `no_action_is_admitted_unless_the_service_runs`, `resolve_joins_the_mapping_with_the_row_and_the_service_status`, `resolve_asks_the_hub_fact_first_and_the_row_before_the_file` |
| 21 (the caller, C1, M2's non-hub answer) | only the hub opens a chat file; folders through the real canonicalizer; retirement | `rust/backend/src/chat_tests.rs`: `only_hub_reads_chat_credentials_from_platform_config` (a non-hub of three kinds and a hub that stops being the hub, through an observing reader), `a_hub_with_missing_or_refused_files_is_off_and_writes_nothing`, `a_refusal_names_the_file_it_came_from`, `a_non_hub_in_a_shared_home_answers_not_hub_where_a_hub_maps_the_folder`, `mappings_are_admitted_against_the_canonical_folders_on_disk`, `a_mapped_folder_with_no_row_at_start_leaves_the_other_projects_mapped`, `a_row_made_after_the_start_in_a_mapped_folder_is_admitted`, `lifecycles_retirement_stops_the_running_service_within_its_bound`, `a_stop_that_does_not_finish_is_reported_incomplete_and_final`, `a_start_under_way_when_retirement_comes_is_ended_with_it`, `retirement_that_began_first_refuses_the_start`, `a_start_that_fails_leaves_the_chat_off_with_its_code_and_a_private_lock_folder_is_given`, `a_dropped_chat_lets_its_listener_go_and_a_start_after_a_stop_is_a_new_chat`, `a_lock_folder_that_cannot_be_made_leaves_the_chat_off_and_registered` |
| 21 (the caller: the reload) | one load path; a reload under the setup lock stops, waits for the old owner's end, loads off the runtime, swaps and starts; nothing starts or is swapped after retirement began; each `SETUP_WAIT` give-up logged; the owner back to idle; one hub fact watch per chat, whatever its units and reloads | `rust/backend/src/chat_tests.rs`: `a_reload_applies_new_tokens_and_a_new_mapping_with_no_daemon_restart`, `a_hub_booted_without_chat_toml_starts_at_its_first_reload_and_retires_with_the_daemon`, `a_reload_after_retirement_began_starts_nothing`, `a_retirement_during_a_reload_stops_what_it_started`, `a_retirement_while_a_reload_waits_for_the_stop_starts_and_swaps_nothing`, `a_holder_of_the_setup_lock_edits_the_files_and_reloads`, `a_reload_whose_stop_outlasts_setup_wait_starts_nothing_and_says_so`, `a_reload_whose_start_outlasts_setup_wait_answers_starting_and_says_so`, `two_reloads_at_once_run_one_after_the_other`, `one_hub_fact_watch_steps_every_workspace_down` (every workspace steps down after two reloads; the count of watches is review's, as no test reads it); `server/chat_watch_tests.rs`: `a_watch_through_a_reload_is_told_and_its_next_watch_is_served_by_the_new_service`, `a_reload_starts_its_service_only_after_the_old_owner_ended`, `an_ended_service_turns_the_chat_off_with_its_code_and_frees_its_lock`; `server/chat_mail_tests.rs`: `the_owner_runs_after_a_reload_with_no_watch` |
| 21 (the caller, B3i1: the mail jobs) | the recipient is the folder's one Claude row else its one Codex row and is absent without a live session; a filing names its row and files nothing after the hub moved; the start's read is every inbox's filings or a failure; the commit makes its folder | `rust/backend/src/chat/mail_tests.rs`: `the_mail_goes_to_the_folders_one_claude_row_else_its_one_codex_row`, `a_handle_moved_before_the_append_is_not_filed`, `a_handle_with_no_live_session_is_absent`, `a_filing_job_files_nothing_once_the_topology_names_another_hub`, `the_starts_read_gives_each_channels_filings_from_every_inbox`, `the_commit_writes_the_boundary_into_a_private_folder_it_makes` |
| 21 (several workspaces) | a unit, an owner and a delivery per workspace; one filing slot, one hub-fact watch, one set of transfer permits per daemon; admission by the target's workspace and the mapping's, so no request reaches Slack under one workspace's token for another's channel or user; each owner delivers, passes over and publishes its own workspace's channels alone; a filing's names looked up before the slot; a refused watch leaves no desk | `rust/chat/src/fake_slack/teams.rs`: the `Teams` fake records every request whose bearer is one workspace's and whose channel or user is another's (`crossed`) and fails the test that drops it holding one, so every two-workspace test below is also a test of no crossing (`a_channel_of_another_workspace_is_no_such_channel` alone crosses on purpose); `rust/backend/src/server/chat_setup_tests.rs`: `two_workspaces_from_nothing_each_project_reads_and_posts_in_its_own` (the plan's done test, the daemon's half; one workspace's publish keeps the other's states), `a_target_naming_another_running_workspace_for_the_rows_channel_is_stale_mapping` (history, post, file, watch and a row's file post, none reaching Slack), `a_channel_of_another_workspace_is_no_such_channel`, `the_workspaces_op_keeps_a_failed_workspaces_name`, `resolve_judges_a_row_by_its_own_workspace_and_an_unmapped_row_by_whether_any_runs`, `a_connect_that_would_push_a_served_workspace_past_the_bound_is_refused`; `server/chat_mail_tests.rs`: `two_workspaces_file_their_own_mail_one_filing_at_a_time` (each post to its own row; the second workspace's filing lands no sooner than `FILE_GAP` after the first's held filing is released, its names looked up while the first held the slot), `a_filing_waiting_behind_another_workspaces_ends_at_once_with_its_delivery`; `server/chat_watch_tests.rs`: `a_refused_watch_of_an_unknown_workspace_leaves_no_desk`; `rust/backend/src/chat_tests.rs`: `a_workspace_whose_start_fails_leaves_the_others_running`, `a_held_workspace_waits_alone`, `retirement_stops_every_workspace_within_its_bound`, `a_retirement_between_register_and_starting_finds_every_workspaces_start`, `a_reload_after_retirement_began_starts_no_workspace`, `a_workspace_with_no_file_is_not_connected_and_its_refused_file_is_its_own`, `one_hub_fact_watch_steps_every_workspace_down`; `rust/backend/src/chat/watch_tests.rs`: `one_channel_id_in_two_workspaces_is_two_slots`, `the_watch_bound_counts_every_workspace`, `an_end_tells_and_clears_only_its_workspaces_slots`, `a_workspaces_status_names_only_its_own_connections_and_channels`, `the_daemons_filings_take_one_slot_file_gap_apart`, `a_dropped_filing_waiting_at_the_slot_files_nothing`; `service_tests.rs`: `two_services_share_the_transfer_permits_they_are_given` |
| 22 | one listener (C14's running half) | `service_tests.rs`: `the_service_api_holds_the_credentials_user_token`, `duplicate_workspace_or_app_has_one_listener` (a second service in this process: `Held`, no call, no socket; start after stop), `a_token_for_another_workspace_is_refused_before_serving`, `stop_is_bounded_final_and_repeatable_and_the_lock_waits_for_the_listener` (a peer that ignores close; the lock held while stopping, free after), `a_handle_kept_across_stop_is_refused_by_every_call`, `the_lock_is_dropped_with_the_task_before_stop_returns` (the abort is awaited), `a_lock_that_cannot_be_made_is_not_held_and_the_codes_are_fixed`, `a_second_concurrent_stop_returns_at_once_and_the_status_reaches_stopped`, `a_stop_before_the_socket_connected_returns`, `a_dropped_service_releases_the_lock_only_with_its_listener`, `a_killed_holder_leaves_the_workspace_free` (a second process; SIGKILL), `a_child_started_by_the_daemon_does_not_keep_the_lock` (Unix only; relies on platform's close-on-exec, which no chat mutation reaches), `a_bot_token_refusal_ends_the_service_and_a_missing_scope_does_not` (a row's post and a read refused by a token code end it `bot_token_refused`; `missing_scope`, a person's refusal, a bare 403 to a read and a post, and a stop do not; the app read at the start, unknown with its code when `bots.info` fails), `an_ended_service_ends_its_watches_wait_and_frees_its_lock` (a hello of another app: the wait in flight `Stopped`, every channel `Needed`, nothing taken, no new `Watches`, `Stopped`, a second service started), `a_refused_join_or_file_step_ends_the_service`, `an_end_after_the_claim_ended_is_no_end_and_the_first_end_stays`; `api_tests.rs`: `the_bots_app_is_read_at_start_and_a_failure_leaves_it_unknown` (one `bots.info` under the bot's bearer for its `bot_id`; a refusal, no app, an app that is no id and a dropped connection each an error that ends nothing; no bot id, nothing asked) |
| 23 | watching (M5) | `watch_tests.rs`: `watch_snapshot_overlap_and_lag_resynchronize` (live edit, delete and add during a snapshot; a new message never replaces; the overlay at its bound and one past it), `a_gap_makes_every_watch_incomplete_until_a_later_snapshot_and_clears_only_then` (a gap pending, a second gap during a resync, a refused clear, the value given), `a_gap_with_nothing_watched_clears_and_a_watch_begun_after_it_needs_no_resync`, `events_nobody_keeps_and_answers_nobody_wants` (an unwatched channel, replies, an unwatch and a re-watch during a snapshot, a failed snapshot, an old page's edit and an unheld delete), `syncing_a_gap_already_known_keeps_a_complete_watch`, `a_snapshot_replaces_what_is_held_and_a_queued_event_never_wins_over_it`, `a_burst_after_the_events_were_taken_refuses_the_snapshot`, `a_gap_read_before_a_newer_one_was_seen_cannot_roll_the_model_back`, `no_interleaving_of_a_snapshot_a_pump_and_the_source_ends_complete_over_a_lost_event` (all 4,200 orders of a snapshot task (begin, fetch, read the gap, answer), a pump task (two pumps, a reconcile, then one more pump when `wait` says look) and the source, two starts, delivered or lost third edit, so 16,800 runs, each through a `Service` over a socket with no task, so the product's own take and wake run: a complete channel is never stale, a gap is never cleared over a stale model), `begin_applies_the_events_it_is_given_to_every_channel_whether_or_not_it_begins`, `a_snapshot_keeps_only_this_channels_top_level_messages`, `timestamps_order_numerically_on_validated_values`, `the_watch_is_bounded_in_channels_ids_and_messages`; `socket_tests.rs`: `take_hands_over_every_pending_event_and_the_gap_and_frees_the_budget`, `wait_wakes_on_a_gap_and_on_an_event_and_takes_nothing` (the event arrives while the wait waits); `service_tests.rs`: `the_watch_clears_the_services_gap_and_a_stopped_service_clears_nothing`, `a_stop_makes_every_watch_owed_a_snapshot_and_a_stranger_clears_nothing`, `a_wait_woken_while_the_stop_is_under_way_answers_false`, `a_dropped_service_ends_its_watch`, `a_stop_under_way_already_ends_the_watch` (a cancelled stop leaves `Stopping`), `loaded_reads_its_own_services_gap_and_a_stop_ends_what_was_pending_and_the_claim`, `a_pump_loop_beside_a_latched_gap_sees_it`, `wait_wakes_for_a_gap_and_an_event_sees_what_is_queued_and_ends_with_the_stop`, `a_wait_in_flight_ends_at_the_stop`; `watch_tests.rs` also: `pump_and_begin_apply_the_services_events_in_the_call_that_takes_them`, `a_service_has_one_watches_and_no_other_reads_its_events` (a stranger's take, answer and wait), `many_threads_asking_at_once_get_one_watches` (8 threads, 3,000 rounds) |
| 24 | mail body, reader, origin | `mail_tests.rs`: `hostile_chat_is_one_marked_mail_record`, `a_mail_body_is_bounded_and_says_it_was_cut`, `an_omitted_record_is_its_header_and_a_cut_ts_line`, `filed_of_reads_only_what_body_writes`, `origin_filter_delivers_people_without_row_loops` |
| 25 | boundary file, filed set | `state_tests.rs`: `the_boundary_is_the_later_of_the_file_and_the_floor_and_a_commit_never_lowers_it`, `an_unreadable_state_file_stops_its_channel` (not a `Ts`, no line end, a second line, over 4 KiB, not UTF-8, a directory, on Unix a FIFO), `a_missing_file_is_the_floor` (no filing moves it; the encoded line end), `the_merge_takes_only_this_channels_filings_above_the_boundary`, `a_set_over_the_bound_keeps_every_filing_above_the_boundary_and_says_so`, `a_ts_at_or_below_the_boundary_is_settled_and_one_above_it_only_when_held`, `the_file_is_named_from_validated_ids_alone` |
| 27 | one queue, filed once, paced, after the start's read; passes owed, the backoff, the commit | `delivery_tests.rs`: `a_post_seen_live_and_by_a_pass_is_filed_once` (taken twice, in flight, out of `ts` order, the same `ts` in two channels, a live event then the pass's result and the reverse, each with a refusal that must not be retried), `live_mail_is_filed_once_and_a_refusal_owes_a_pass`, `an_omitted_live_record_owes_a_pass_and_is_not_filed`, `filings_are_at_least_file_gap_apart_from_a_return`, `nothing_is_filed_or_passed_before_the_starts_read_is_in`, `the_queue_and_a_channels_filed_set_are_bounded_and_a_dropped_post_owes_a_pass`, `a_post_at_or_below_the_boundary_is_never_queued_and_owes_nothing`, `a_returns_report_is_true_once_when_it_takes_the_set_past_its_bound`, `a_pass_is_owed_after_every_newer_gap_a_held_or_dropped_filing_and_by_the_sweep` (the sweep with nothing filed, one pass for one gap, `read_pace`, `Order.seen`), `the_commit_moves_the_boundary_to_h_or_to_the_last_filing_before_the_first_that_was_not` (the four cases, the `Truncated` limit and its owed pass), `a_commit_never_reaches_a_post_still_pending`, `a_commit_that_leaves_a_candidate_unfiled_backs_off`, `pass_candidates_never_take_the_queue_past_its_bound`, `a_live_filing_says_it_is_live`, `a_full_filed_set_is_drained_by_its_owed_pass`, `a_channels_failing_passes_never_starve_another` (a channel skipped or failing every time, another passed once), `a_gap_during_a_pass_owes_another`, `the_sweep_runs_from_the_last_completed_pass`, `a_pass_owed_by_a_refusal_is_not_owed_again_once_it_has_completed`, `backoff_doubles_to_its_cap_and_a_complete_pass_resets_it` (also a rate limit and a skipped pass) |
| 28 | the setup's checks | `api_tests.rs`: `identify_answers_the_tokens_workspace_and_no_error_carries_a_token` (another workspace answered, `bot_id` dropped when invalid, one bearer request with no workspace sent, each `Malformed` case, Slack's code, an echoed token, 500, a 429 that refuses no later call, a dropped connection; no error shows the token or a URL); `socket_tests.rs`: `an_app_token_check_opens_no_socket_and_refuses_a_foreign_url` (one POST under the app token per check, a Slack URL accepted, a foreign or `ws` URL `destination`, a URL over 2 KiB, no URL and HTML `malformed`, `ok: false` `invalid_auth` and 401 `rejected`, `ratelimited` and 503 `unavailable`; no upgrade asked and no socket ever open; no error shows the token, the ticket or a host) |
| 29 | the picker's calls | `api_tests.rs`: `the_channel_list_follows_the_cursor_to_its_bound_and_fails_whole` (the request's parameters and cursor, skipped entries, the five-page cap and `more`, a repeated or oversized cursor, a failure or a rate limit on a later page, a page of the wrong shape), `a_channels_facts_come_from_one_read_and_the_naming_rule_reads_them` (the facts, the defaults for what Slack leaves out, a refused id unsent, and `author_name` and `channel_names` agreeing with them), `a_join_is_sent_once_and_already_in_channel_is_joined` (each refusal, each unconfirmed answer, none resent, a bad id and a closed client unsent) |
| 30 | the window's pairing | `server/chat_setup_tests.rs`: `a_window_pairs_a_row_with_a_public_channel_the_app_joins_and_watches_it_at_once`, `a_private_channel_the_app_is_not_in_is_refused_unjoined`, `a_join_without_the_scope_is_not_in_channel` (a refused join and an unacknowledged one, each sent once), `unpairing_removes_the_mapping_and_the_channel_and_the_row_reads_unmapped`, `a_channel_of_another_existing_folder_is_taken_and_one_of_a_missing_folder_is_taken_over`, `a_pair_that_changes_nothing_writes_and_reloads_nothing` (a hand comment survives), `a_delivering_files_new_mapping_floors_at_the_clock`, `a_hand_edited_file_the_load_refuses_is_never_rewritten`, `a_hub_deposed_since_its_boot_writes_nothing`, `the_channel_list_needs_a_running_service_and_a_window`; `dispatch_place_tests.rs`: the two ops are jobs |
| 32 | a file shared once | `api_tests.rs`: `with_no_person_token_a_persons_post_is_refused_unsent_and_a_refused_token_is_forgotten` (a person's share with no token refused, nothing sent), `a_persons_file_share_uses_their_token_and_carries_the_comment_alone` (steps 1 and 3 under the person's bearer, step 2 under none, the escaped comment alone or no `initial_comment`; a token refused at step 1 or 3 forgotten, the next share unsent), `a_file_is_uploaded_unauthorized_and_shared_once_with_its_label` (three requests in order, step 1's name and length, the bytes equal and step 2 without authorization, step 3's file, channel, thread and escaped label; the label alone with no comment; every input refused unsent, the bounds themselves carried, a closed client), `only_the_share_can_be_unknown` (a drop or a timeout at step 1 or 2 refused with no share sent; a drop, a timeout, a 503 or an `ok` without the file at step 3 unknown, each sent once), `a_share_with_no_ts_is_posted_with_its_file` (and a private channel's share), `a_foreign_upload_url_is_refused_before_any_byte_and_nothing_is_shared` (`other.example`, two lookalikes, a URL over 2 KiB; one connection), `a_third_transfer_is_busy_at_once_and_a_dropped_one_frees_its_permit`, `a_file_post_shows_no_token_or_private_url_in_any_error` (sentinels in the upload URL and the token, echoed by each step), `a_rate_limit_refuses_a_file_post_unsent` (at step 1, and the wait refusing the next unsent; at step 3, sent once) |
| 31 (the user token) | the user token's absence, form, refusals, other workspace and transport; nothing written; the done test of a person's post | `server/chat_setup_tests.rs`: `each_answer_of_the_user_tokens_check_has_its_code_and_writes_nothing`, `a_hub_whose_secrets_hold_no_user_token_says_so_and_posts_nothing_for_a_person`, `a_window_connects_three_tokens_and_its_post_goes_out_as_the_person`; `server/chat_ops_tests.rs`: `a_persons_post_slack_refuses_is_not_a_member_or_forgets_the_token`, and the extended `each_api_error_has_its_stated_code`; `config_tests.rs`: `a_secrets_text_holds_the_three_tokens_and_parses_back` |
| 31 | a window's only; the hub fact at the op; the three tokens checked, nothing written before; only Slack's answers that name the token refuse it, its state and the transport are `failed`; the closed mark read again under the lock; another workspace and a refused `chat.toml` refused and left as they are; the secrets owner-only and whole, a fresh `chat.toml` only when missing; a failed write `not_saved`; the reload; no token byte in a log line of any thread or a reply; the reply after a failed start; `resolve`'s `unconfigured` | `rust/backend/src/server/chat_setup_tests.rs`: the plan's done test `from_nothing_a_window_connects_pairs_and_reads_its_channel` (from no chat file a window connects, `chat.toml` is the fresh one, lists, pairs, watches and gets a socket post as `chat.changed`; the secrets owner-only and the four keys, `chat.toml` loads with the mapping, no token byte in a reply, an event or a log line of any thread), `each_answer_of_the_two_checks_has_its_code_and_writes_nothing` (the forms, the token refusals and Slack's state and the transport on each check), `a_connect_from_a_row_or_on_a_computer_that_is_not_the_hub_writes_nothing`, `a_retirement_that_begins_while_slack_is_asked_writes_nothing`, `a_write_that_fails_is_not_saved`, `a_connect_whose_start_fails_is_saved_and_resolve_says_start_failed`, `resolve_says_why_a_service_ended` (`wrong_app`, `tokens_refused` for each refused token), `a_connect_of_another_workspace_adds_it`, `a_connect_that_would_push_a_served_workspace_past_the_bound_is_refused`, `resolve_says_unconfigured_for_each_reason_a_connect_fixes`, `a_connect_while_running_replaces_the_tokens_and_restarts`; `socket_tests.rs`: `an_app_token_check_opens_no_socket_and_refuses_a_foreign_url` (a 503 and `ratelimited` are `unavailable`); `dispatch_place_tests.rs`: `the_lists_are_disjoint_and_reasoned` (the setup ops are jobs) |
| 33 | a picture of the channel, bounded | `api_tests.rs`: `a_fetch_reads_only_a_picture_shared_in_its_channel` (the thumbnail and the original with the bearer, to the file's host; another channel's file, a file shared nowhere, a PDF, an SVG, a direct message's share and an original over the bound each with no GET; a JPEG, a GIF and a WebP; a thumbnail of a large original or with no `size`, an original with no `size` `Malformed`; both membership forms and a private share; `url_private` without a download URL; a file Slack does not know, an `info` for another file, a bad id, a closed client and one closed while `files.info` was answered, which sends no GET; the `Debug` of a result), `a_thumbnail_falls_back_to_a_small_original_and_never_to_a_large_one` (the three keys' order; a small original at exactly `MAX_THUMB`; one byte over `NoThumbnail` with no GET), `an_answer_that_is_not_an_image_is_refused` (HTML, octet-stream, `image/` alone, SVG, icon, `image/jpg` and no type refused; a type in capitals with parameters, JPEG, GIF and WebP accepted; no bytes), `each_variant_is_read_under_its_own_deadline` (a stalled thumbnail times out at a read's deadline and a stalled original at the transfer's), `a_fetch_is_bounded_while_it_streams` (a false `size`; one byte over and exactly the bound, both variants), `a_fetch_redirected_off_slack_sends_nothing_there` (two foreign hosts, no request reaches them; a redirect within Slack keeps the bearer), `a_fetch_shares_the_transfer_permits_and_a_rate_limit_waits_the_next_read` (two stalled originals make a fetch and a post `busy` at once; a dropped one frees its permit; a 429 on the URL), and the token test `a_file_post_shows_no_token_or_private_url_in_any_error` (extended: a fetch's errors) |

## Files
- `Cargo.toml`: the manifest; the dependencies are listed at the head of this page.
- `src/lib.rs`: the modules and re-exports.
- `src/normalize.rs`: the normalization. `src/config.rs`: the configuration; `src/config_tests.rs`: its tests.
- `src/http.rs`: the HTTP core and `Http::websocket`. `src/api.rs`: the Slack calls; `src/api_tests.rs`: their tests.
- `src/socket.rs`: the Socket Mode listener; `src/socket_tests.rs`: its tests.
- `src/service.rs`: the service; `src/service_tests.rs`: its tests.
- `src/watch.rs`: the watch model; `src/watch_tests.rs`: its tests.
- `src/mail.rs`: the mail body, its reader and what is mail; `src/mail_tests.rs`: its tests.
- `src/state.rs`: a delivering channel's boundary file and filed set; `src/state_tests.rs`: its tests.
- `src/delivery.rs`: the filing queue, the passes' schedule and the commit; `src/delivery_tests.rs`: its tests.
- `src/fake_slack/`: the fake (tests and the `test-support` feature), its websocket peer, its history, upload and file scripts and its certificates. `src/http_tests.rs`: the HTTP tests.

## Start here
`src/normalize.rs`, `normalize`; `src/http.rs`, `Http::send`; `src/api.rs`, `Api::history` and `Api::post`; `src/socket.rs`, `Worker::run` and `Worker::handle`.
