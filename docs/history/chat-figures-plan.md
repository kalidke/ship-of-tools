# Figures in the project chat: a plan (replaces B4)

Written 2026-11-16 against main `f69766dc6`, by reading only: nothing was built or run, no Slack service was contacted,
no secrets file was read. Public-safe: no host, user or workspace is named. Committed with the first landing (L1);
`docs/package.md`'s State names it as the plan for figures in place of B4. The chat contract's guarantees 30 and 31
were the setup's by the time L1 landed, so the guarantee this plan calls 30 (L1) is the contract's 32, and the one it
calls 31 (L5) will be the next free number.

## 1. The goal, and where the chat stands

The maintainer asked (2026-11-15) whether figures work in the project chat and whether a session can post a figure to
its project's channel. They do not, today:
- No upload exists. `Api` has no file call (`rust/chat/src/api.rs`: `post`, `history`, `scan`, `channels`, `channel`,
  `join`, names), the row's tool posts text only (`agents/sot-fe/sot-fe-chat.sh`, `send_chat_post`), and wire says
  "`chat.fetch` and `chat.post_file` are B4's" (`rust/protocol/CLAUDE.md`, guarantee 12).
- No file is ever fetched. A record carries file summaries and no URL (`rust/protocol/src/chat.rs:159-171`; the
  `thumbnail` flag is set from Slack's `thumb_*` keys, `rust/chat/src/normalize.rs:395`); the drawer shows each file
  as a line `[file <name>, <size>]` (`rust/frontend/src/ui/drawer/chat/project.rs:144-150`); mail says `not fetched`
  (`rust/chat/CLAUDE.md`, guarantee 24).

The stage makes these true, each landing leaving the product working:
1. A row's agent posts a file (a figure first: PNG, SVG, PDF; any regular file up to the bound) into its channel, in a
   thread or not, with `sot-fe chat post --file <path>`, under the rules a row's text post has today: its own bound
   handle, one post per call, exit 3 when the post may exist.
2. The Chat drawer shows a message's image files as pictures (a thumbnail fitted in a tile), and a person opens one in
   the preview pane.
3. A person posts the file the preview pane shows from the window (small, so planned; it lands last).

Whether the maintainer's question is his word to start figures is his to say: `docs/package.md` says figures "wait for
the maintainer's word after the first stage is measured". This plan starts only on that word.

## 2. Constraints

- Never contact a real Slack in a test or a landing; every test runs against the fake Slack (`rust/chat/src/fake_slack/`).
- No token in a log, an error, a wire payload, mail or any file but `chat-secrets.toml`. No private URL (a file's
  `url_private*`, `thumb_*` or an upload URL) leaves `sot-chat`.
- A mutation is sent once and never resent; an unconfirmed one is `unknown` (chat guarantee 12). A row's tool exits 3
  when the post may exist and never posts again.
- Nothing from a channel reaches a terminal; a channel's file is untrusted bytes.
- One change per landing; each new rule has one test, red before the change; the contract or manual page changes in the
  commit that changes the code it describes. American spelling. The repo is public: no private names in commits or docs.

## 3. Premises

Checked by reading, with the evidence:
- **P1 The app's manifest carries `files:read` and `files:write`.** True: `rust/protocol/src/chat.rs:60`
  (`SLACK_APP_MANIFEST`), pinned by `chat_tests::the_manifest_has_socket_mode_on_rotation_off_and_its_url_decodes_to_it`
  (`rust/protocol/src/chat_tests.rs:215-245`, which lists both). An app made from an older manifest lacks them and
  answers `missing_scope`, which the plan reads as a refusal (nothing posted, no picture); no hub has a chat
  configuration yet, so the first app is made from this manifest.
- **P2 `files.slack.com` is a Slack host, and the authorization rides only to Slack hosts.** True: `is_slack_host`
  admits `slack.com` and its dot-boundary subdomains (`rust/chat/src/http.rs:483-488`); every hop, the first and each
  redirect, is checked before a connection (`http.rs:372`, `check_url` at `425-437`: `https`, no userinfo, port 443,
  a Slack host); the bearer is built anew for each admitted hop and marked sensitive (`http.rs:375-380`). A redirect is
  followed by a GET only, at most three (`MAX_REDIRECTS`), each hop checked again; a redirect to another host is
  `destination` before any request there, so the token never reaches it; a redirected POST is `redirect`
  (`http.rs:388-399`). A request carries no bearer unless `.bearer()` is called (`Request::post`, `http.rs:152-167`).
  Live (the maintainer's workspace, 2026-11-01): every private download seen, a PNG original, a phone JPEG's original
  and its `thumb_360`, was one hop to `files.slack.com`, 200, authorization sent, no redirect
  (`docs/history/slack-client-plan.md:839`, `:1010`).
- **P3 The upload is `files.getUploadURLExternal`, a POST of the bytes to the returned URL, then
  `files.completeUploadExternal` with the channel and an optional `thread_ts`.** Measured live, not read from Slack's
  documentation (which this work did not fetch, since its host is Slack's): SlackClaw's shelved Julia client ran these
  three steps against the maintainer's workspace on 2026-11-01 and the file was shared to the channel (SlackClaw's
  `dev/output/slack-api-facts.md`, section 2 and its gate results; `docs/history/slack-client-plan.md:1011`): step 1 a
  GET with `filename` and `length` answering `file_id` and `upload_url`; step 2 a POST of the raw bytes,
  `application/octet-stream`, with no authorization; step 3 a JSON POST of `files: [{id, title}]`, `channel_id`,
  `initial_comment` under the bot token, the only step that shares. Slack strips a PNG's text chunks; pixels round-trip,
  bytes do not (`slack-client-plan.md:1011`). That `files.upload` is retired is from memory of Slack's announcements,
  unverified here; nothing in this plan uses it.
- **P4 The fixture can serve every host the plan needs.** True: the fake's resolver and certificate cover `slack.com`,
  `api.slack.com`, `files.slack.com` and the foreign `other.example` and `evilslack.com`
  (`rust/chat/src/fake_slack/mod.rs:39-47`; `server.pem`'s subject alternative names), so no new certificate is made
  (its CA key was discarded).
- **P5 A row's file can be named the way the preview names one, links followed, no `..`.** True: a `files:<rel>` id
  is composed beneath the row's root with `..` and absolute parts refused (`rust/backend/src/files/tree.rs:230-256`),
  and the read resolver follows links, by design, so `data/results` linked to the NAS works
  (`tree.rs:124-142`); the drawing's figure is read this way (`rust/backend/src/agents/selection.rs:209-245`).
  `open_regular` opens a regular file only, without a FIFO's wait on Unix (`rust/platform/src/host/regular.rs`;
  platform contract line 123).
- **P6 The window decodes PNG, JPEG, GIF and WebP.** True: `image = "0.25"` with default features
  (`rust/Cargo.toml:52`), decoded by `with_guessed_format` (`rust/frontend/src/ui/preview/image/png.rs`), whose ceiling
  is 4 GiB (`MAX_DECODE_ALLOC`), too high for a channel's bytes; this plan bounds chat pictures before that decoder.
- **P7 The preview can show bytes with no node, and a newer request outdates a reply in flight.**
  `InstalledPreviewSource::install` takes `node_id: Option<String>` (`rust/frontend/src/ui/preview/source.rs`);
  `PreviewRequests::issue_preview` advances the generation `admits_preview` checks (`preview/requests.rs:48-56`,
  `71-77`); `forget_fired` makes "the next cursor follow fetch the cursored node again" (`requests.rs:175`), so the
  open must not call it. The drawing already refuses a picture with no node ("Can't draw here: there is no image file
  to draw on.", fe-ui contract).
- **P8 Chat requests run as jobs off the connection's loop.** True: an op in neither `INLINE` nor `POOLED` is a job
  (`rust/backend/src/server/dispatch_place_tests.rs:17-19`), and a handler returns `(Frame, Option<blob>)`
  (`rust/protocol/src/reply.rs:12`), as `preview.get` does with its bytes.

Live premises, not checkable before the first commit without a real Slack. Each is built to fail closed and visibly,
with the failure pinned by a test against the fake; each is checked at a person's first use on a release candidate (or
by a live gate run on the maintainer's word), and its landing record says "unmeasured" until then:
- **LP1 `upload_url` is on a Slack host.** If not, step 2 is `destination` before any byte leaves and nothing is shared
  (exit 2). Test: `api_tests::a_foreign_upload_url_is_refused_before_any_byte_and_nothing_is_shared`. Fallback: a
  reviewed amendment to chat guarantee 5 admitting that host for an unauthenticated POST only (its limit already names
  "an upload host").
- **LP2 `thread_ts` on step 3 shares into the thread.** Not seen live (the gate shared to the channel). If Slack ignores
  it, the figure lands at the top level; the reply still says `posted`.
- **LP3 The share's `ts` is in step 3's answer** (`files[0].shares.public|private[<channel>][0].ts`; the facts note says
  it can be absent). Absent is `posted` with the file id and no `ts`, never `unknown`.
- **LP4 The share read back is a `file_share` message from the bot with the comment as its `text`.** If not, L2's
  label finds nothing and the share reads as `Bot` (an `APP` post, never mail): safe.
- **LP5 `files.info` for a bot token lists the channels a file is shared in** (`channels`, `groups`, or
  `shares.public|private`; coded and tested in the shelved client, not seen live). If not, every fetch is refused
  `bad_request` and tiles stay empty. Fallback: check the file id against the records the daemon's watch model holds
  for the target channel instead (`Watches::messages`).
- **LP6 PNG, GIF and WebP files get `thumb_360` (or 480, 160) served as `image/*`.** Seen live for a JPEG only. If not,
  a small original (at most `MAX_THUMB`) stands in; a larger one leaves its tile empty.

## 4. The plan of record's B4, and the smaller design

`docs/history/slack-client-plan.md` (section 4, its B4 row, "B4's replacement for P12", and rules C9, C10, F1 to F6)
planned figures as files: a managed `.sot-chat/` store in each mapped project, reservations bounding its bytes,
materialization of each fetched original or thumbnail, closed-leaf eviction by rename and unlink, refetch by file id,
the window opening those files through Files nodes and a docs grant for files over the preview cap, and `show-result
--post`; it needed P3 (substitution-safe managed directories on three systems), P7 (watcher exclusions) and P8 (bounded
memory reads in fe-net) first.

The two goals need none of the store. A figure goes up from bytes the daemon reads once into memory, and comes down as
one bounded reply the window holds in memory. Nothing is written to any disk, so there is nothing to reserve, evict,
exclude from the watcher, ignore in git or protect against a substituted directory.

Kept from the plan of record: C9's three steps with the share as the only mutation (its source-change checks are
replaced by reading the whole file before step 1, so the bytes sent are the bytes read); C10's checks on a fetch (the
file shared in the target's channel, its size before and while it streams, the answer's kind, so Slack's sign-in page
is never a picture); the thumbnail bounds of U3; one connection per host (M7); the label in `initial_comment` (a file
share takes no custom author or metadata).

Not needed: P3, P7, P8 (the daemon is trusted and bounds its own reply, as `preview.get` does today; the window checks
the length it read), F1 to F4, F5's docs path, F6 and `show-result --post` (the one route is `sot-fe chat post --file`).

What the smaller design gives up, by decision:
- A chat picture is not a project file: no drawing on it for the agent, no crop, scale, annotation or open in another
  program, and it does not outlive the window. Each open fetches the original from Slack again (at most 16 MiB), and
  each window session fetches its thumbnails again.
- Only PNG, JPEG, GIF and WebP are pictures. A PDF, an SVG or any other file stays a file line in the drawer and is not
  opened from it. (A row still posts any regular file.)
- A file over 16 MiB is neither posted nor opened.
- An agent cannot see a person's inbound figure: mail still lists it as `not fetched`. A row's fetch command is not in
  this stage.
- No byte-equality promise: Slack keeps a PNG's pixels, not its text chunks.

## 5. Decisions closed

- **D1 Two new ops, never a field on `chat.post`.** `chat.post_file` and `chat.file`. A field on `chat.post` would be
  ignored by an older daemon, which would post the comment alone (wire guarantee 3's trap); an unknown op is refused
  whole (the tool prints `refused`, exit 2; fe-net reads it as `Unsupported`).
- **D2 The wire names a file by its `files:<rel>` id**, the one name of a project file on the wire (`preview.get`,
  `selection.set`). The tool makes it from the path it is given: a relative path is joined to `$PWD` without resolving
  links, and the result must lie under `$SOT_WORKSPACE_ROOT/` as written; `rel` is the rest. The daemon composes it
  beneath the row's root through `FilesMode::node_id_to_path` (refuses `..` and absolute parts; follows links, so a
  `data/results` link to the NAS posts). The read is `open_regular` and at most `MAX_CHAT_FILE` plus one byte, on the
  blocking pool.
- **D3 The chat's own configuration folder is never posted.** A file whose canonical path lies in platform's
  `config_dir()` (canonical), which holds `chat-secrets.toml`, is refused `not_a_file`, even through a link inside the
  project. Other files a link inside the project reaches can be posted: rows share the daemon's account, so the folder
  rule is attribution and accident, not a security boundary (as D13 is).
- **D4 Who posts as whom is `chat.post`'s rule, one function for both ops:** a `person` post only from a window
  connection, a `row` post only under the row's bound handle, each `bad_request` before any Slack call.
- **D5 The label.** `initial_comment` is the escaped text of `<name> (<kind> via Ship of Tools)`, followed by `: ` and
  the comment when there is one, made Slack's by the one private escape (`slack_text`), the bound on the escaped whole
  (`MAX_TEXT`). The file's `title` and `filename` are its base name. **Read back (L2),** an own `file_share` message whose
  text starts with that label (escaped as sent) takes its origin and author name from it, after `metadata` and the
  `username` suffix, and its text is what follows the label. So a row's figure is `Row` and never mail, and a person's
  (D11) is `Drawer` and mail, as their text posts are. Only own posts are read so; a member's typed label is text.
- **D6 Outcomes.** Steps 1 and 2 share nothing, so any failure there, a timeout included, is `refused` (exit 2). Step 3
  is read by `acknowledgement` as `chat.post` is: `ok` with a valid `files[0].id` is `posted {file, ts?}` (`ts` when
  the share's is there, LP3), `ok` without one is `unknown`; Slack's definite refusal is `refused`, a 429 `limited`,
  anything else `unknown`. Step 1 is a read: a `Retry-After` in force refuses the post `limited`, unsent.
- **D7 At most two transfers at once** (`MAX_TRANSFERS`, an upload or a fetch, per daemon), inside `Http`'s sixteen;
  the third is `busy` at once. A permit is held across all of an operation's steps and freed on every path, a dropped
  future included.
- **D8 A fetch reads one file of the target's channel.** `chat.file {target, file, variant}` with `variant` `thumb` or
  `original`: one `files.info`; the file must be shared in `target.channel` (else `bad_request`, nothing downloaded),
  its `mimetype` one of `image/png`, `image/jpeg`, `image/gif`, `image/webp` (else `not_an_image`), its `size` within
  the variant's bound (else `too_large`, unsent); the URL is `thumb_360`, `thumb_480`, `thumb_160`, else the original
  when its size is at most `MAX_THUMB`, for a thumbnail, and `url_private_download`, else `url_private`, for the
  original; one GET with the bearer, bounded while it streams; an answer whose `Content-Type` is not `image/*` (Slack's
  sign-in page) is `not_an_image`. The reply carries the bytes as the frame's blob.
- **D9 The drawer's tiles.** Each file of a record whose MIME is one of the four is a square tile; the tiles of a
  record form a band after its text and meta lines, wrapped to the body's width, so a record's height follows from its
  image count, the width and the text scale alone, before any byte arrives, and an anchored reader never moves when a
  thumbnail lands. A tile shows its thumbnail fitted and centered, or an empty bordered box. The band holds no text: the
  selection, copy, lead and mark rules are unchanged, and the mark's band covers the record's whole height.
- **D10 Opening a picture.** A left press on a tile opens that file; `chat.open_image` (`Primary+o`, Chat scope) opens
  the marked message's first image. One open in flight; a newer one replaces the wanted file and a reply for another is
  dropped. The window asks `chat.file original`, checks the header is at most 40 megapixels (the daemon's own drawing
  bound), then advances the preview's request generation with `issue_preview` (never `forget_fired`, which would
  refetch the cursored node at once), and installs the bytes with no node through the path `install_preview_reply`
  takes (`InkLayer::install_preview`), so the canvas hides and drawing, crop, scale and open-in-program act on nothing.
  A later cursor move, a row switch or a `preview.changed` for the fired node shows a file again.
- **D11 A person's file post (goal 3)** is the file the preview pane shows: its installed node, of the row the drawer
  shows, posted with `chat.post_file` of kind `person` under `[chat] name`, into the draft's destination (else the
  watched channel, or the open thread), with the draft's text as the comment. It takes the one post slot text posts use;
  only `posted` clears the draft. `chat.post_file` (`Primary+u`, Chat scope). A preview with no node (a chat picture,
  nothing shown) is refused on the status line, nothing sent.
- **D12 Refusals of a thumbnail.** `not_an_image`, `too_large` and `bad_request` are final for the window session;
  `failed` is asked again at most three times, 5 s apart; `busy` and `unavailable` on the owner's `PACE`; `limited` after
  its wait.
- **D13 Codes.** Three are added to `ChatCode`: `not_a_file` ("the file is not a readable file in the row's folder"),
  `too_large` ("the file is larger than the chat carries"), `not_an_image` ("the file is not a picture the chat
  shows"). An older reader reads each as `other`.

## 6. Bounds, caches and their removal

| what | bound | held where | gone when |
|---|---|---|---|
| a posted file | 1 byte to `MAX_CHAT_FILE` (16 MiB), regular, UTF-8 base name of at most `MAX_FILENAME` (256) bytes with no control character | daemon memory, read once; `Http` clones a POST body for its one hop, so 32 MiB at most | the reply |
| a comment | the escaped label and comment within `MAX_TEXT` (8 KiB) | the request | the reply |
| `upload_url` | at most 2 KiB, a Slack host on 443 | inside `Api::post_file` | the call's end |
| transfers at once | `MAX_TRANSFERS` 2 per daemon; at most 64 MiB of file bytes | `Api` | each permit at its operation's end |
| deadlines | step 1 and `files.info` 15 s (reads); step 2 and an original's GET 60 s (`TRANSFER_DEADLINE`); a thumbnail's GET 15 s; step 3 30 s (a post) | `Http`'s owner-held deadline | — |
| JSON answers | `MAX_BODY` 1 MiB | — | — |
| a thumbnail's bytes | `MAX_THUMB` 256 KiB | the reply frame | the window's decode |
| an original's bytes | `MAX_CHAT_FILE` 16 MiB | the reply frame, then the installed preview source and the row's parked view, as any preview | the next install, or the row's view dropped |
| images a record shows | the record's eight files (`MAX_FILES`) | — | — |
| a thumbnail decoded | header at most 2048 x 2048 before decoding, `max_alloc` 32 MiB, held downscaled to at most 360 px on its long side (about 0.5 MiB RGBA) | window memory, with its texture | past 64 held, the least recently placed leaves first; the window's end |
| thumbnail requests | 2 in flight per host | the window | the reply or the link's end |
| an opened original | header at most 40 megapixels | the preview | as any preview |
| on disk | nothing, anywhere | — | — |

A token rides step 1, step 3, `files.info` and the file's GET to a Slack host, and nothing else; step 2 carries none.
`HttpError` and `ApiError` carry codes only; the daemon logs codes; no URL is in any value that formats.

## 7. Landings

Order and dependencies: L1 then L2 (the chat crate, `api.rs` and `normalize.rs`); L3 (wire) beside them; L4 after L1
and L3 closes goal 1 (L2 before it, so a row's figure reads back as its own). L5 (chat crate) after L1; L6 after L3 and
L5; L7, L8, L9 in order after L6 close goal 2. L10 after L4 and L9 (it reuses the handler and the drawer's open path)
closes goal 3, and is the first to drop if the maintainer prefers. Lines are planned, product / test.

### L1 chat: a file is shared once (`Api::post_file`), 140 / 220
Rule (chat guarantee 30, new): `Api::post_file(channel, thread, kind, name, file_name, bytes, comment)` makes the three
steps of D6 under one transfer permit (D7): step 1 a read (`files.getUploadURLExternal`, `filename`, `length`); step 2
`Request::post(upload_url, TRANSFER_DEADLINE, "application/octet-stream", bytes)` with no bearer; a `closed` check; step 3
`files.completeUploadExternal` (JSON, bearer, `POST_DEADLINE`) read by `acknowledgement`. Input it cannot carry (empty
or over-bound bytes, a bad name, channel or thread, a comment over the bound) is `Err` unsent. The fake Slack gains the
two methods and an upload path on `files.slack.com` that records the raw body and whether an `Authorization` header
came. Files: `rust/chat/src/api.rs`, `api_tests.rs`, `fake_slack/mod.rs`, `lib.rs` (constants), `rust/chat/CLAUDE.md`.
Tests, red first: `a_file_is_uploaded_unauthorized_and_shared_once_with_its_label` (three requests in order, the bytes
equal, no authorization on step 2, `channel_id`, `thread_ts`, `title` and the escaped label in step 3),
`only_the_share_can_be_unknown` (a timeout or drop at step 1 or 2 is refused and step 3 never sent; at step 3 unknown,
never resent), `a_share_with_no_ts_is_posted_with_its_file`,
`a_foreign_upload_url_is_refused_before_any_byte_and_nothing_is_shared` (LP1, `other.example`),
`a_third_transfer_is_busy_at_once_and_a_dropped_one_frees_its_permit`,
`a_file_post_shows_no_token_or_private_url_in_any_error` (sentinels in `upload_url` and the token),
`a_rate_limit_refuses_a_file_post_unsent`.
Records: chat contract guarantee 30, guarantee 5's limit (an unauthenticated POST to a Slack upload host), the head
paragraph's runtime state (the transfer permits), Owns and the Tests table; `docs/defects.md` opens `CHAT-FIGURES`
(chat, [feature], the maintainer's question of 2026-11-15); this plan to `docs/history/chat-figures-plan.md`;
`docs/package.md` State.
Traps: never call `.bearer` on step 2; a failure before step 3 is `Refused`, never `Unknown`; the label goes through
`slack_text`, the only way a body gets text (guarantee 12); the permit is an owned permit moved into the future; the
`upload_url` and `file_id` are checked (`valid_id`, 2 KiB) before use and never formatted.

### L2 chat: a file share reads back as its poster's, 30 / 90
Rule (chat guarantee 2, amended): D5's read-back. Files: `rust/chat/src/normalize.rs`, `normalize_tests.rs`,
`api_tests.rs`, `rust/chat/CLAUDE.md`. Tests: `an_own_file_share_takes_its_origin_from_its_label_and_a_members_does_not`
(own row and person labels, a label with no comment, a member's and another bot's labeled `file_share`, an own text post
whose text starts with a label, which `metadata` decides), `a_file_share_read_back_through_history_keeps_its_origin`
(L1's share answered by the fake's history). Traps: match the label as sent, escaped; only `file_share` own posts;
after `metadata` and `username`; a text halved to fit the record bound may lose the label and read as `Bot` (safe).

### L3 wire: the file pair, 80 / 140
Rule (wire guarantee 12, amended): `chat.post_file {target, thread?, kind, name, node_id, comment?}` (`node_id` at most
4096 bytes, `comment` at most `MAX_TEXT`) answering `ChatPostFileRes`: `posted {file, ts?}`, `refused {code}`,
`limited {retry_after_secs, assumed}`, `unknown` (also any outcome added later); `chat.file {target, file, variant}`
(`file` an `is_id`, `variant` `thumb` or `original`) answering `ChatFileRes {mime, blob}` with `blob.len` at most
`MAX_CHAT_FILE`; `MAX_CHAT_FILE` (16 MiB) and `MAX_THUMB` (256 KiB) defined here once; D13's codes. Both names in
`op_table!`. Files: `rust/protocol/src/ops/chat.rs`, `ops/mod.rs`, `ops/chat_tests.rs`, `rust/protocol/CLAUDE.md`
(its line "`chat.fetch` and `chat.post_file` are B4's" replaced). Tests: `the_file_pair_keeps_its_shape_and_its_bounds`,
`a_file_outcome_or_code_added_later_reads_as_the_safe_one`, and the extended
`the_chat_ops_are_declared_in_the_one_table` and `every_code_has_its_one_text`. Trap: the decoder bounds the blob's
declared length; the codec has read it before the typed decode, so the window checks it against its request (L6).

### L4 daemon and agents: a row posts a file (goal 1), 110 / 200
Rule: `chat::ops::post_file` decodes, admits (`admitted`), applies D4 through the function `post` now shares, resolves
the row's `FilesMode`, composes `node_id` (D2), refuses D3, reads the file on the blocking pool (`not_a_file`,
`too_large`), calls `Api::post_file` once and maps the outcome. `sot-fe chat post [--thread <ts>] --file <path>
[<comment> | --stdin]` makes the node id (D2, refused exit 2 before an endpoint when the path is not under the row's
folder), resolves, sends one `chat.post_file` of kind `row`, waits 120 s, and prints `posted <ts>` or
`posted file <id>` (exit 0), `refused <code> <words>` or `limited, wait <n> s` (exit 2), or the may-exist words (exit 3).
Files: `rust/backend/src/chat/ops.rs`, `server/dispatch.rs`, `server/dispatch_place_tests.rs` (both new ops are jobs),
`server/chat_ops_tests.rs`, `server/chat_mail_tests.rs`, `agents/sot-fe/sot-fe-chat.sh`, `agents/sot-fe/sot-fe`
(flag and usage), `agents/tests/test-sot-fe-chat.sh`, `comm/adapters/claude/sot-comm/SKILL.md` and
`comm/adapters/codex/skills/sot-comm/SKILL.md` (one line each: `--file`), `comm/legacy-skills.txt`.
Tests: `chat_ops_tests::a_row_posts_a_file_of_its_folder_under_its_own_handle_once` (D4's two refusals, a stale row
read and sent nothing), `a_file_outside_the_folder_or_in_the_config_folder_is_never_read_or_sent` (`..`, absolute, a
FIFO, a directory, empty, over 16 MiB, a link in the project to the config folder; a link to a folder outside the
config folder posts), the shell file cases (outside a row, outside the folder and no handle refused before an
endpoint; one resolve then one `chat.post_file` with the node id, thread and comment; each outcome's words and code;
no answer exit 3), and the goal's done test below.
Records: `agents/CLAUDE.md` (the `sot-fe chat post` paragraph and guarantee 9's op list), `rust/backend/src/server/CLAUDE.md`
(the chat jobs), `docs/ownership.md` (the chat ops row), `docs/src/guide/chat.md` (a row's figure), chat contract
guarantee 21's D13 paragraph (both ops).
Traps: admit before reading; `node_id_to_path`, never the confined resolver (it refuses NAS links); the tool joins a
relative path to `$PWD`, never `pwd -P`; bash 3.2; one request, no retry; `comm/legacy-skills.txt` is regenerated by
`scripts/legacy-skill-hashes.sh` after the commit (agents guarantee 8; `test/protect_tests.jl` is red until then). Limit
to record: a Windows hub's tool compares Git Bash's `/c/...` with `C:\...` and refuses (nothing sent); the shell test
runs on Linux and macOS bash 3.2, not Git Bash.

### L5 chat: a file of the channel is fetched (`Api::fetch_file`), 110 / 200
Rule (chat guarantee 31, new): D8, under a transfer permit. The fake gains `files.info` and file paths on
`files.slack.com`. Files: `rust/chat/src/api.rs`, `api_tests.rs`, `fake_slack/mod.rs`, `rust/chat/CLAUDE.md`.
Tests: `a_fetch_reads_only_a_picture_shared_in_its_channel` (another channel's file, a PDF, a size over the bound:
each refused with no GET), `a_thumbnail_falls_back_to_a_small_original_and_never_to_a_large_one`,
`an_answer_that_is_not_an_image_is_refused` (200 `text/html`), `a_fetch_is_bounded_while_it_streams` (a false
`size`, a body one byte over), `a_fetch_redirected_off_slack_sends_nothing_there` (to `other.example`: `destination`),
and the L1 token test extended to fetches. Traps: membership before any download; the URLs never leave the function;
the bound is the request's own (`max_body`).

### L6 daemon and fe-net: the bytes cross the wire, 90 / 150
Rule: `chat::ops::file` admits, calls `Api::fetch_file` once, and answers `(frame, Some(bytes))` with `blob.len` equal;
fe-net's `OutgoingReq::ChatFile` and `ChatEvt::File { target, file, variant, reply }` take the frame's blob, check its
length against the variant's bound (else `Refused(Failed)`), are not replayable and get no event when their connection
ends (the owner asks again at `Connected`), and an older daemon's unknown-op answer is `Unsupported`. Files:
`rust/backend/src/chat/ops.rs`, `server/dispatch.rs`, `server/chat_ops_tests.rs`,
`rust/frontend/src/net/transport/ops/chat.rs`, `transport/request.rs`, `transport/event.rs`, `transport/reply.rs`, their
tests, `rust/frontend/src/net/CLAUDE.md`. Tests: `chat_ops_tests::a_file_reply_carries_its_bytes_as_its_blob` (and a
stale row fetches nothing), `ops::chat::tests::a_file_reply_carries_its_request_and_its_bytes`,
`a_file_read_without_its_answer_ends_with_no_event_and_is_never_written_again`. Trap: the other chat replies carry no
blob, so the chat decoder must be handed the frame's blob for this op alone.

### L7 fe-ui: the pictures owner, 120 / 180
Rule: `drawer/chat/pictures.rs`, `ChatPictures`, fields private: keyed by host, workspace, channel and file id; asks
for the thumbnails of placed records' image files, at most 2 in flight per host; decodes a reply on the UI thread
within section 6's bounds (header first, then `Limits`), downscaled to 360 px; holds at most 64, the least recently
placed leaving first; D12's refusals; a link change clears that host's in-flight set. Files: `pictures.rs`,
`pictures_tests.rs`, `drawer/chat/drawer.rs` (it holds the owner and returns its requests), `rust/frontend/src/ui/CLAUDE.md`
(Owns and the Chat owner section). Tests: `at_most_two_thumbnails_are_asked_per_host_and_none_twice`,
`a_thumbnail_past_its_bounds_is_never_decoded_whole`, `the_least_recently_placed_leaves_past_64`,
`a_refusal_is_final_or_asked_again_by_its_code`, `a_link_change_asks_again_what_was_in_flight`.

### L8 fe-ui: the band of tiles, 120 / 140
Rule: D9. `ChatView`'s record height adds the band; `tiles()` gives each tile's rectangle and file; `backs` draws each
empty tile's border clipped to the body; the pass draws each held thumbnail's texture fitted in its tile under the
body's scissor, after the cell backgrounds and before the text; a texture is made at first draw and dropped with its
entry. Files: `drawer/chat/view.rs`, `view_tests.rs`, `render/pass/` (the chat section), `native_fixture_tests.rs`,
fe-ui contract. Tests: `a_records_band_follows_its_image_count_width_and_scale`,
`a_thumbnail_arriving_moves_nothing` (anchored and following), `the_band_is_never_selected_or_copied`,
`every_tile_is_inside_the_body`. Limit to record: the textures and their draw order are pinned only by the native
fixture.

### L9 fe-ui: open in the preview (goal 2), 80 / 120
Rule: D10. Files: `drawer/chat/drawer.rs`, `view.rs` (`tile_at`), `input/keybindings.rs` (`chat.open_image`),
`drawer/chat/mod.rs` and `preview/` (State's install), `requests_tests.rs`, `thread_tests.rs` or a new `open_tests.rs`,
fe-ui contract, `docs/src/guide/chat.md`, `docs/src/ref/` keybindings. Tests: `a_press_on_a_tile_opens_that_file_and_selects_nothing`,
`the_key_opens_the_marked_messages_first_image`, `one_open_is_in_flight_and_a_late_reply_is_dropped`,
`a_picture_over_40_megapixels_is_not_installed`, `requests_tests::a_chat_picture_outdates_the_reply_in_flight_and_keeps_the_fired_target`.
Records: `docs/defects.md` moves `CHAT-FIGURES` to Closed. Traps: `issue_preview`, never `forget_fired`; install through
`InkLayer::install_preview` with no node; a press on a tile starts no selection.

### L10 fe-net and fe-ui: a person posts the previewed file (goal 3), 110 / 150
Rule: D11. fe-net's `OutgoingReq::ChatPostFile` is not replayable; a connection that ends first, a `handler_error` or an
answer that does not parse is `unknown`, never refused. Files: `net/transport/ops/chat.rs` and its tests,
`drawer/chat/owner.rs`, `compose.rs`, `drawer.rs`, `input/keybindings.rs`, `net/CLAUDE.md`, fe-ui contract,
`docs/src/guide/chat.md`. Tests: `compose_tests::the_previewed_file_posts_once_with_the_draft_as_its_comment`,
`a_preview_with_no_node_posts_nothing`, `ops::chat::tests::a_file_post_without_its_answer_is_unknown_and_never_written_again`.

## 8. Done tests

- Goal 1: `server::chat_mail_tests::a_rows_figure_reaches_its_thread_once`: a daemon on the fake Slack, a row in a
  mapped folder and a PNG in it; a connection as the row sends `chat.post_file` into a thread; the fake records one
  step 1, one step 2 whose bytes are the file's and which carries no authorization, and one step 3 naming the channel,
  the thread and the row's label; the reply is `posted`; the share read back through history is `Row` under the row's
  name and is not mail. With it, `agents/tests/test-sot-fe-chat.sh`'s file cases.
- Goal 2: `drawer::chat::native_fixture_tests::a_chats_pictures_show_and_one_opens_in_the_preview` (under a display):
  records with image files; the window asks thumbnails at most two at a time; each answered with a PNG; every frame
  presents and the anchored record does not move; `Primary+o` sends one `chat.file original`; answered, the preview's
  installed source is the picture with no node.
- Goal 3: `compose_tests::the_previewed_file_posts_once_with_the_draft_as_its_comment`.
- The stage is done when a person has used it on a release candidate with the hub's chat configured, which is also
  when LP1 to LP6 are first seen.

## 9. Records the stage changes

`rust/chat/CLAUDE.md` (guarantees 2, 5, 21, new 30 and 31, Owns, the head paragraph, Tests), `rust/protocol/CLAUDE.md`
(guarantee 12), `agents/CLAUDE.md`, `rust/backend/src/server/CLAUDE.md`, `rust/frontend/src/net/CLAUDE.md`,
`rust/frontend/src/ui/CLAUDE.md`, `docs/package.md` (State, the chat paragraph's ops), `docs/ownership.md`,
`docs/src/guide/chat.md`, the keybindings reference, the two `sot-comm` skills and `comm/legacy-skills.txt`,
`docs/defects.md` (`CHAT-FIGURES`), `docs/history/passes.md` (each landing's record), and this plan in `docs/history/`.
`docs/history/slack-client-plan.md` is history and is not edited.
