# rust/backend/src/pages: pages (charter)

## Idea

The daemon serves a few pages to the OS browser from loopback listeners (a video player page, a static site or
doc), and a window on another machine reaches them through the one control tunnel it already holds, by
`proxy.connect`. Nothing is served that an op did not grant, and every address a page or the proxy uses is the port
actually bound, not the preferred one. Part of the daemon (rust/backend).

## Owns

- The video listener and its token grants (`video.rs`).
- The site prefix and pool listeners and their tables (`site/`), the
  `git` child of a site open, and the bounds of a tracked link (`site/links.rs` `Bounds`).
- `proxy.connect` and its loopback allowlist (`proxy.rs`).

## Promises

- Every listener binds 127.0.0.1 and falls back to an ephemeral port when the preferred one is taken. URLs and the
  allowlist read the bound port (`bound_video_port`, `bound_site_port`, `pool_assigned_ports`, `bound_pluto_port`),
  never the preferred one.
- Pluto contributes only its current supervisor's READY grant to `allowed_proxy_ports`; after that owner releases the grant, a new proxy request cannot use it. Existing pipes and the lookup-to-connect race are not listener-identity proofs.
- Nothing is served that an op did not grant: video by token (`register_video`), a prefix site by nonce (`set_root`),
  a pool site by its per-open secret and then an HttpOnly cookie (`assign_pool_port`).
- The video, site and pool listeners are bound non-inheritable (`bind_page_listener`), so no child process holds one,
  and accept through `serve_own`: only this OS account's connections are served.
- Minting fails closed: `random_token` returns `None` rather than a weak token.
- `proxy.connect` dials only 127.0.0.1 ports in `allowed_proxy_ports`, with a 5 s connect bound, and logs a refused
  port once per streak.
- The video, site and pool servers keep a connection for the next request (HTTP/1.1), so a remote window's page costs
  one channel of its host's link per browser connection, not one per file, and no login of its own; a connection closes when its request says so, when a reply
  cannot be sent whole, or after 30 s with no whole request head or POST body, or with a 64 KiB reply write the client takes more than 30 s over (`read_request`, `read_body`, `IDLE_BOUND`, `WRITE_BOUND`).
- A window's page channel ends when its host's link ends before the daemon's replies: the browser connection closes
  as on a down link (`pipe_one`, `Answer::LinkDown`), and while the link is down nothing is opened. A page already
  piped ends with the link too, even one whose browser stopped reading: its copies select on the link's end. The
  channel's open waits at most `LINK_REPLY_BOUND` (10 s), then fails `OpenError::TimedOut`, and the channel a late
  answer opens is closed.
- `proxy.connect` dials with tokio's `TcpSocket`, whose socket is not inheritable, so no child holds the connection
  (`dial_upstream`, `a_proxy_dial_is_not_inheritable`).
- A window's sites outlive its connections: `docs.open` keys them to the window, its hello's host and `instance`
  (`ClientGuard::site_owner`), so a redial keeps them, and they go when the roster forgets the window; a connection
  whose hello names no window has its sites go when it disconnects (`remove_root`; ADR 0029, 0.6.6 amendment). When
  every pool port is held, a new open takes the port of the window away longest, never a live window's
  (`assign_pool_port`, `Clients::away_sites`). A daemon restart ends every grant: the tables are memory only.
- A window keeps its last eight prefix sites (`SITES_PER_WINDOW`): opening a question's page leaves the queue page
  live. The ninth open evicts the oldest nonce; `remove_root` removes every nonce of the owner. The pool still keeps
  one site per owner, so opening a second pool site replaces the first.
- An authenticated opener sets one entry's reply scope with `DocsOpenReq.direct`: `none` (the default), `row` with a
  workspace id, or `all`. A row request resolves an id or slug to the canonical workspace id and refuses a page outside
  that row's canonical project folder, or a row root holding the home folder, with a reason and no URL. The folder's
  index must also stay inside that row. An all-row grant retains ordinary docs confinement to a known workspace.
  Scope belongs to the entry's URL path (a folder URL and its index are the same entry), never the site tree. A document
  or POST body cannot set or upgrade it. `states.reply_scope` reports `{scope: "none"}`, `{scope: "row", workspace_id}`
  or `{scope: "all"}` from the requesting capability and entry, even when an older frontend discarded the opener's field.
- Navigation from a scoped entry to another HTML document exchanges its capability through a noncached redirect:
  a prefix document gets an ordinary nonce (counted in the eight-site limit); a pool document gets an ordinary secret
  on the same origin, keeping root-relative assets working. That secret and the entry secret expire together on
  reassignment. A cookie still authorizes files only. An ordinary capability stays ordinary on navigation back to the
  original entry; the entry's own grant keeps its scope. A POST elsewhere beneath a direct nonce has scope `none`.
  A reply from an entry with no scope offers the window's bar and answers 202. A reply from a scoped entry needs no
  window: a `row` scope naming any other row's handle is refused `403 out_of_scope`; otherwise rows' direct delivery
  (`PageVerbs::with_direct`) types it at once or holds it, and the POST answers `200 {ok, receipt}`, the receipt being
  `{request_id, workspace_id, handle, outcome, detail}` with `outcome` `sent`, `held`, `refused` or `unknown`. The page
  may name its `request_id` (1 to 64 of `A-Za-z0-9-_`); a repeated id answers its receipt again and types nothing.
  A scoped POST's deadline is 15 s; a page that misses it reads the outcome from `states.replies`, rows' newest
  receipts (memory only: after a daemon restart a held reply has none, and its page shows unknown). Every `reply` POST
  logs one INFO line (target `page_reply`, "page reply answered": the handle, the entry's scope, the status and the
  answer), whether it was offered to the bar, given a receipt or refused, so an answer that never landed can be found
  (`site/verbs_tests.rs::a_scoped_reply_is_delivered_directly_and_a_row_scope_answers_only_its_row`).
- A site's POST channel admits a live site capability first (prefix nonce or explicit pool secret), then requires
  `Origin` to equal `http://` plus the request's own `Host`. Missing, null, repeated or foreign origins are refused
  with 403 `forbidden`; a missing nonce is a file 404. No command runs before these checks. GET and HEAD remain files;
  all other methods, OPTIONS included, answer 405. Remote proxies preserve the browser's Host and Origin, so a window's
  local proxy port needs no daemon-side mapping.
- Pool POSTs require the per-open `?secret=` explicitly; cookies authorize only file GET/HEAD. A page posts to
  `location.pathname + location.search`, retaining the secret of its own open. Reassignment replaces the secret,
  so a surviving old tab cannot act for the new owner even when the browser sends the new tab's cookie. The captured
  owner and nonce/secret are checked again after blocking work, before returning states or sending a command.
  POSTs never set a file cookie, so an earlier grant's slow verb cannot overwrite a newer document's cookie.
- A POST body requires one valid `Content-Length`, at most 4 KiB, read within 30 s (`IDLE_BOUND`). Missing or invalid
  lengths and transfer encoding answer 411; oversize answers 413; an incomplete body answers 400, a timed-out body
  408. These refusals close the connection. A consumed body leaves the connection open unless requested closed,
  including after a bad JSON object or verb. Verb answers are JSON with `Cache-Control: no-store`: success has
  `ok: true`, refusal has `error` and `code`.
- One `PageVerbs` supplies three bounded budgets shared by all prefix and pool listeners: four POST body readers,
  four `states` handlers and four action handlers (`goto`, `open`, `reply`), each `MAX_IN_FLIGHT`. The body
  reader releases its slot once it has consumed the bounded body, before the decoded verb takes its own handler
  slot. Excess work answers 503 `server_busy`: a body admission refusal closes without reading, while a handler
  admission refusal has consumed its body and keeps the connection unless requested closed. A handler waits at most
  5 s (`HANDLER_BOUND`), then answers 503 `handler_timeout`. Blocking work retains its own class's slot until it
  actually ends, including after that answer; no late command is sent. Four stalled actions never spend a polling
  slot, and four stalled polls never spend an action slot. A stalled registry can occupy all four polling slots
  indefinitely; new polls are then refused, while action capacity and file requests remain independent.
- `states` returns this daemon's registry sessions, including sessions outside rows, sorted by handle. Foreign and
  unknown-host entries are omitted. Rows owns the question parsing and the binding to a local row. States needs no
  live window. An unreadable, missing or malformed registry (including a missing/non-object `agents`) answers JSON
  500 `handler_error`, never a successful empty queue. `goto` resolves a handle fresh and sends
  `goto_workspace {workspace}`. `open` takes only the handle and reads the question's `Page:` fresh. Its absolute
  page must canonicalize under that row's canonical project root, which must not hold the home folder. It grants a
  prefix site with that canonical row's reply scope, rooted at that row (no git link authority or widened URL root), then sends `docs {workspace,
  path: "sot-page:<nonce>/<relative>"}`. `docs.open` redeems only the requesting owner's live grant, returning its
  existing URL through the usual remote proxy route, without interpreting the path under any other workspace.
  The open verb grants only the target row, whatever the opener's scope or POST fields; redemption preserves that
  existing grant even if its request asks for a wider scope. On Unix a decision site holds the row's `PinnedDir` from preparation through the grant's lifetime, after checking
  that its identity matches the row's original folder. Every file request refuses a changed base identity and opens
  each component with `openat` from the borrowed pin descriptor (`AsFd`), never by reopening the base pathname.
  This works on Linux and macOS, even if the row or an ancestor is replaced between the check and the final open;
  links below the pin are not followed. `.git` and `..` remain refused. An extra caller-supplied path grants nothing.
- Windows keeps the existing canonicalize-then-`File::open` path. A file or intermediate folder replaced between
  those steps can escape confinement; this landing does not close that race. Platform's stated directory-pin gap
  ([guarantee 10](../../../platform/CLAUDE.md#guarantees)) accepts an ordinary file on Windows, and the pin supplies
  no Windows handle-relative descendant open. The Unix descriptor walk above makes no Windows guarantee.
- Decision pages must use relative asset and navigation paths, such as `assets/app.js`. Root-relative paths such as
  `/assets/app.js` escape the prefix nonce and normally answer 404: decision grants do not support them. This limit
  applies to decision grants; ordinary `docs.open` retains its existing pool routing.
- A page command goes by target serial to the owner's newest live named frontend (`PageVerbs.window`), never the
  window in use. Go-to and open answer 202; no live window answers 503 `window_gone` and sends nothing. No local row
  answers 409 `not_a_row` (a known foreign entry names its daemon); no question page answers 409 `no_page`; an outside,
  relative or unresolvable page answers 403 `page_outside`. Unknown or malformed verbs answer 400 `bad_request`. A blocking handler failure is logged and answers JSON 500 `handler_error`, keeping the
  consumed connection available for the next request. `reply {handle, text}` resolves the handle with the same local/foreign rule, then calls wire's `page_reply::line`.
  Invalid text answers 400 `bad_text`, naming the refusal; valid text sends `page_reply {workspace, handle, line}` to
  the same owner connection and answers 202. A POST offers only: it never types or confirms delivery, and the daemon
  keeps no pending-reply table. A window shows the offer on its reply bar (fe-ui); an older window drops the command and delivers nothing.
  `fe.command.send` refuses the reserved command, so only this page server publishes an offer.
- A quarto render starts through `Signal::spawn` and is always given the daemon's julia as `QUARTO_JULIA`, an
  inherited one overridden, and a refusal by the resolver fails the render (`run_quarto`), `--no-execute` or not,
  since quarto runs `QUARTO_JULIA` for an `engine: julia` document even then; so a host without julia renders no
  document through quarto. The site's `git` child runs through `Signal::spawn_std` (`run_git`).
- A `quarto render`, `--no-execute` or not, ends within 30 min (`QUARTO_RUN_BOUND`: Quarto 1.9 starts its Julia engine either way, and installs it on a first use), its tree killed and the error (and the warn in the log) naming the bound and the last five lines it wrote to stderr; one render of a document runs at a time; each render writes its own output file.
- A window's relay page proxy opens the same generated relay endpoint as its control connection, with a handoff hello and proxy.connect before the existing byte splice.
- A remote window serves a page a daemon op returned (`docs.open`, `video.open`, `pluto.open`) on a loopback port the
  OS assigns it, one per host and daemon port, and opens the URL with only the port replaced; `proxy.connect` names the
  daemon's port (`bind_proxy_listener`, `with_loopback_port`, `PageListener`, `armed_url`). A URL user code announced
  or `open_url` named keeps the daemon's number and is refused when that number is taken (`PageSource`; ADR 0035,
  PAGE-PORT update).
- A loopback port a REPL child's `browser` frame announces is dialable through `proxy.connect` only after the REPL's
  grant is applied and answered, for that child's life: the table holds, per row, the newest child generation that has
  announced and its ports, refuses an event of an older generation (`PortRefused::Stale`) and drops an older child's
  ports when a newer one's first event arrives; it keeps at most 16 ports a row and one empty entry per row until the
  daemon ends (`PagesGrants`, which the server installs through `sidecars::ports::install`; sidecars' R6).
- `.git` and `..` are never served, and a link is followed only when git tracks it, its target is written as an
  absolute real path under a mount folder (outside the repository; the home folder and the repository by folded names), and the file, reached by its written names with no symlink below the link, lies under `/mnt`, `/media` or `/Volumes`, outside the
  home folder, with no hidden part below that folder (`site/` `resolve_and_open`, `Bounds`).
- `docs.open` serves a file, or a folder's `index.html`, from the outermost folder holding an `index.html` between it
  and the top of its repository or the workspace root, else from its own folder. No site root holds the home folder:
  a page whose own folder does is refused (`find_site_root`).
- Rust video opens, preview bounds and HTTP MIME decisions use sot_protocol::video_path::video_mime; leading-dot filenames follow the existing Julia suffix contract.

## Connections

Each connection is one row of docs/integration.md, owned by its provider. Provides: `video.open`, `docs.open`,
`quarto.open`, `quarto_render_error`, `proxy.connect`, `ensure_proxy_for_url`, `pipe_one`, `PagesGrants`,
`is_servable_video`, `start_page_servers`, `PageVerbs`, `PageRow`, `PageOption`, `RowRef`, `remove_root`, `loopback_port_from_url`, `with_loopback_port`,
`rust/protocol/src/page_url.rs`. Uses wire's `page_reply::line` for reply offers.
Uses: `LinkHandle` (topology's link), `proxy.connect`, `handle_connection`, `handle_proxy_connect`, `sot_protocol::pipe::pipe_bidirectional`, `sot_protocol::reply::reject`,
`site_owner` and `away_sites` (the window's key and the windows away, given to `handle_docs_open` as arguments),
the server-built `PageVerbs` closures (states, handle lookup, window lookup and the fe-command bus), Unix
`sot_platform::host::{PinnedDir, dir_identity}` (`AsFd` for the pin's borrowed descriptor), `Signal::spawn`, `Signal::spawn_std`, `Contained`, `ContainedStd`, `Signal`, `sot_containment::process`, `resolve_bin`,
`sot_state_dir`, `host_name`, `state_dir_hash`, `bound_pluto_port`, `allowed_proxy_ports`, `lane_dial`, `ResolvedDial`, `sot_protocol::video_path::video_mime`.

## Folders

- `site/`: the static-site server: grant tables, listeners, the request and the link rule.

Elsewhere: rust/frontend/src/pages.rs (the window's page proxy).

## Files

- `http.rs`: the request and response code both loopback servers share: request heads and persistent connections,
  the bounded POST body, content types, single ranges, file bodies, plain and JSON replies.
- `mod.rs`: declares the folder's modules, `start_page_servers`, which binds the listeners at boot, and `bind_page_listener`, the one bind of a page listener.
  It also holds `random_token`, the one minter of video tokens, site nonces and pool secrets.
- `ops.rs`: video.open, docs.open (and its site-root walk), quarto.open and its render-failure text.
- `ops_error_tests.rs`: direct cross-platform tables for Quarto failure selection, bounded ERROR retention and transport guidance; one Unix real-handler flood stand-in through the shared `test_isolated::run_isolated` owner checks reply code, no blob, one render, partial-HTML removal and the real writer/decoder boundary.
- `proxy.rs`: `proxy.connect`, the loopback allowlist, the browser-port table and its receiver (`PagesGrants`).
- `site/`: the static-site server (own page).
- `site/verbs.rs`: the page channel and its four server-supplied capabilities; it reads no row registry or roster.
- `site/grant.rs`: entry reply scopes and the ordinary pool credential used for other document navigation.

Page channel tests: `tests/page_channel.rs` drives a real daemon, `docs.open`, separate windows and their redials,
HTTP capability/Origin checks, states, go-to, page containment, site retention and persistent bodies. The bounded
body deadline is `http_keep_tests.rs::a_body_dripped_past_the_bound_is_refused_at_the_bound`; complete owner cleanup
is `site/tests.rs::an_owner_keeps_eight_prefix_sites_and_removes_them_together`. The browser-origin done test remains
a by-hand check on local and remote windows. A page cannot act on another daemon's rows; a session outside any row
has states but no go-to, open or reply. The page channel's trust boundary is **this OS account**, the same as every other
daemon operation (the maintainer's decision, 2026-10-25). A same-account process holding a site capability can call its verbs; possession and matching Host/Origin do
not authenticate that process as the originating document. Other accounts are refused by `serve_own`.
`site/verbs_tests.rs` observes capability calls and commands on denied requests and JSON failure recovery on one
connection. `tests/page_channel.rs::page_verbs_work_through_the_page_proxy_with_the_windows_local_origin` drives the
real `proxy.connect` pipe with browser-facing Host and Origin values different from the daemon's port.
The round-two regressions are `a_reassigned_pool_port_refuses_the_old_document_with_the_new_cookie`,
`a_decision_page_replaced_after_the_verb_cannot_escape_its_row`, and
`a_registry_failure_is_visible_and_recovers_on_the_next_read` in `tests/page_channel.rs`, and
`page_work_is_limited_and_a_timeout_keeps_its_slot_until_the_worker_ends` in `site/verbs_tests.rs`.
`site/verbs_tests.rs::an_expired_grant_cannot_finish_a_verb_that_was_already_reading` expires a grant while its
registry read is held, then observes refusal when that read ends.
`tests/site_windows.rs::a_window_that_redials_keeps_its_pages` checks redial retention through the ninth prefix open.
`tests/page_channel.rs::a_decision_grant_refuses_a_row_folder_replaced_after_the_grant` replaces the granted row
with a link and then another directory at the same pathname (Unix).
`site/links_case_tests.rs::a_decision_file_open_uses_the_pin_after_a_row_or_ancestor_swap` swaps the base or its
ancestor after the identity check and observes the real final-open walker still reading the original row's bytes.
`site/verbs_tests.rs::polling_has_its_own_bounded_budget_and_does_not_block_actions` covers the polling budget;
the existing worker-deadline case now holds four actions and observes successful polls before and after timeout.
`site/verbs_tests.rs::incomplete_bodies_have_a_bounded_budget_that_releases_on_eof` covers the shared body readers.
- `video.rs`: the video listener, its token grants and the request handler.

## Start here

`proxy.rs` `allowed_proxy_ports` for what a remote window may reach; `video.rs` `serve_request` for a served request.

Reply behavior: `tests/page_channel.rs::a_reply_with_hostile_text_is_refused_and_opens_no_bar` observes refusal and
exact marked offers on the owner's newest connection; `a_handle_that_binds_no_row_here_is_refused` includes replies
and named foreign-daemon refusals. `tests/comm_wake.rs::a_page_reply_post_types_nothing` observes an offer and no pane
input for three seconds on a free stub-agent row.
Landing A's scope proofs in `tests/page_channel.rs`: `states_reports_the_entry_scope_with_a_canonical_row_id`,
`a_direct_row_open_refuses_a_page_in_another_known_workspace`, `the_open_verb_always_grants_the_targets_row_and_never_all`,
`navigation_from_a_direct_entry_gets_an_ordinary_grant_in_both_servers`, and
`a_post_cannot_change_scope_and_only_an_unscoped_reply_offers_the_bar`. Direct delivery: `site/verbs_tests.rs::
a_scoped_reply_is_delivered_directly_and_a_row_scope_answers_only_its_row`.
