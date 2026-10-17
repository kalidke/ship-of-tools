# rust/backend/src/pages: pages (charter)

## Idea

The daemon serves a few pages to the OS browser from loopback listeners (a video player page, a static site or
doc), and a window on another machine reaches them through the one control tunnel it already holds, by
`proxy.connect`. Nothing is served that an op did not grant, and every address a page or the proxy uses is the port
actually bound, not the preferred one. Part of the daemon (rust/backend).

## Owns

- The video listener and its token grants (`video.rs`).
- The site prefix and pool listeners and their tables (`site/`), the
  `git` child of a site open, and the `<config>/data-roots` read.
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
  cannot be sent whole, or after 30 s with no whole request head, or with a 64 KiB reply write the client takes more than 30 s over (`read_request`, `IDLE_BOUND`, `WRITE_BOUND`).
- A window's page login, over ssh or a relay, ends when its host's link closes before the daemon's replies: the
  browser connection closes as on a down link (`pipe_one`, `LinkWatch`). A page already piped ends on its own.
- `proxy.connect` dials with tokio's `TcpSocket`, whose socket is not inheritable, so no child holds the connection
  (`dial_upstream`, `a_proxy_dial_is_not_inheritable`).
- A window's sites outlive its connections: `docs.open` keys them to the window, its hello's host and `instance`
  (`ClientGuard::site_owner`), so a redial keeps them, and they go when the roster forgets the window; a connection
  whose hello names no window has its sites go when it disconnects (`remove_root`; ADR 0029, 0.6.6 amendment). When
  every pool port is held, a new open takes the port of the window away longest, never a live window's
  (`assign_pool_port`, `Clients::away_sites`). A daemon restart ends every grant: the tables are memory only.
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
- `.git` and `..` are never served, and a link is followed only when git tracks it and its target lies under a
  declared data root (`site/` `resolve_and_open`).
- `docs.open` serves a file, or a folder's `index.html`, from the outermost folder holding an `index.html` between it
  and the top of its repository or the workspace root, else from its own folder. No site root holds the home folder:
  a page whose own folder does is refused (`find_site_root`).
- Rust video opens, preview bounds and HTTP MIME decisions use sot_protocol::video_path::video_mime; leading-dot filenames follow the existing Julia suffix contract.

## Connections

Each connection is one row of docs/integration.md, owned by its provider. Provides: `video.open`, `docs.open`,
`quarto.open`, `quarto_render_error`, `proxy.connect`, `ensure_proxy_for_url`, `pipe_one`, `record_browser_port`, `revoke_browser_ports`,
`is_servable_video`, `start_page_servers`, `remove_root`, `loopback_port_from_url`, `with_loopback_port`,
`rust/protocol/src/page_url.rs`.
Uses: `LinkGate`, `proxy.connect`, `handle_connection`, `handle_proxy_connect`, `pipe_bidirectional`, `reject`,
`dispatch`, `ClientGuard::site_owner`, `Clients::away_sites`, `Signal::spawn`, `Signal::spawn_std`, `Contained`, `ContainedStd`, `Signal`, `child_signal::process`, `resolve_bin`,
`sot_state_dir`, `sot_config_dir`, `host_name`, `state_dir_hash`, `bound_pluto_port`, `allowed_proxy_ports`, `lane_dial`, `ResolvedDial`, `sot_protocol::video_path::video_mime`.

## Folders

- `site/`: the static-site server: grant tables, listeners, the request and the link rule.

Elsewhere: rust/frontend/src/pages.rs (the window's page proxy).

## Files

- `http.rs`: the request and response code both loopback servers share: request heads and persistent connections,
  content types, single ranges, file bodies, plain replies.
- `mod.rs`: declares the folder's modules, `start_page_servers`, which binds the listeners at boot, and `bind_page_listener`, the one bind of a page listener.
  It also holds `random_token`, the one minter of video tokens, site nonces and pool secrets.
- `ops.rs`: video.open, docs.open (and its site-root walk), quarto.open and its render-failure text.
- `ops_error_tests.rs`: direct cross-platform tables for Quarto failure selection, bounded ERROR retention and transport guidance; one Unix real-handler flood stand-in through the shared `test_isolated::run_isolated` owner checks reply code, no blob, one render, partial-HTML removal and the real writer/decoder boundary.
- `proxy.rs`: `proxy.connect`, the loopback allowlist, browser-port records.
- `site/`: the static-site server (own page).
- `video.rs`: the video listener, its token grants and the request handler.

## Start here

`proxy.rs` `allowed_proxy_ports` for what a remote window may reach; `video.rs` `serve_request` for a served request.
