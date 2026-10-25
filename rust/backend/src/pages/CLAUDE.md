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
  one ssh login per browser connection, not one per file; a connection closes when its request says so, when a reply
  cannot be sent whole, or after 30 s with no whole request head or POST body, or with a 64 KiB reply write the client takes more than 30 s over (`read_request`, `read_body`, `IDLE_BOUND`, `WRITE_BOUND`).
- A window's page login, over ssh or a relay, ends when its host's link closes before the daemon's replies: the
  browser connection closes as on a down link (`pipe_one`, `LinkWatch`). A page already piped ends on its own.
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
- A site's POST channel admits a live site capability first (prefix nonce, or pool secret/cookie), then requires
  `Origin` to equal `http://` plus the request's own `Host`. Missing, null, repeated or foreign origins are refused
  with 403 `forbidden`; a missing nonce is a file 404. No command runs before these checks. GET and HEAD remain files;
  all other methods, OPTIONS included, answer 405. Remote proxies preserve the browser's Host and Origin, so a window's
  local proxy port needs no daemon-side mapping.
- A POST body requires one valid `Content-Length`, at most 4 KiB, read within 30 s (`IDLE_BOUND`). Missing or invalid
  lengths and transfer encoding answer 411; oversize answers 413; an incomplete body answers 400, a timed-out body
  408. These refusals close the connection. A consumed body leaves the connection open unless requested closed,
  including after a bad JSON object or verb. Verb answers are JSON with `Cache-Control: no-store`: success has
  `ok: true`, refusal has `error` and `code`.
- `states` returns this daemon's registry sessions, including sessions outside rows, sorted by handle. Foreign and
  unknown-host entries are omitted. Rows owns the question parsing and the binding to a local row. States needs no
  live window. `goto` resolves a handle fresh and sends `goto_workspace {workspace}`; `open` takes only the handle,
  reads the question's `Page:` fresh, and sends `docs {workspace, path}` only after the absolute page canonicalizes
  under that row's canonical project root, a link out refused. An extra caller-supplied path grants nothing.
- A page command goes by target serial to the owner's newest live named frontend (`PageVerbs.window`), never the
  window in use. Go-to and open answer 202; no live window answers 503 `window_gone` and sends nothing. No local row
  answers 409 `not_a_row` (a known foreign entry names its daemon); no question page answers 409 `no_page`; an outside,
  relative or unresolvable page answers 403 `page_outside`. Unknown or malformed verbs, `reply` included until landing
  2a, answer 400 `bad_request`. A blocking handler failure is logged and answers JSON 500 `handler_error`, keeping the
  consumed connection available for the next request. This landing never types or opens a reply bar.
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
`rust/protocol/src/page_url.rs`.
Uses: `LinkGate`, `proxy.connect`, `handle_connection`, `handle_proxy_connect`, `sot_protocol::pipe::pipe_bidirectional`, `sot_protocol::reply::reject`,
`site_owner` and `away_sites` (the window's key and the windows away, given to `handle_docs_open` as arguments),
the server-built `PageVerbs` closures (states, handle lookup, window lookup and the fe-command bus), `Signal::spawn`, `Signal::spawn_std`, `Contained`, `ContainedStd`, `Signal`, `sot_containment::process`, `resolve_bin`,
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

Page channel tests: `tests/page_channel.rs` drives a real daemon, `docs.open`, separate windows and their redials,
HTTP capability/Origin checks, states, go-to, page containment, site retention and persistent bodies. The bounded
body deadline is `http_keep_tests.rs::a_body_dripped_past_the_bound_is_refused_at_the_bound`; complete owner cleanup
is `site/tests.rs::an_owner_keeps_eight_prefix_sites_and_removes_them_together`. The browser-origin done test remains
a by-hand check on local and remote windows. A page cannot act on another daemon's rows; a session outside any row
has states but no go-to or open. Other processes of this OS account already have the control socket and are outside
the page-script boundary.
`site/verbs_tests.rs` observes capability calls and commands on denied requests and JSON failure recovery on one
connection. `tests/page_channel.rs::page_verbs_work_through_the_page_proxy_with_the_windows_local_origin` drives the
real `proxy.connect` pipe with browser-facing Host and Origin values different from the daemon's port.
- `video.rs`: the video listener, its token grants and the request handler.

## Start here

`proxy.rs` `allowed_proxy_ports` for what a remote window may reach; `video.rs` `serve_request` for a served request.
