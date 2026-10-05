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
- Nothing is served that an op did not grant: video by token (`register_video`), a prefix site by nonce (`set_root`),
  a pool site by its per-open secret and then an HttpOnly cookie (`assign_pool_port`).
- Every listener accepts through `serve_own`: only this OS account's connections are served.
- Minting fails closed: `random_token` returns `None` rather than a weak token.
- `proxy.connect` dials only 127.0.0.1 ports in `allowed_proxy_ports`, with a 5 s connect bound, and logs a refused
  port once per streak.
- A connection's sites go when it disconnects (`remove_root`).
- `.git` and `..` are never served, and a link is followed only when git tracks it and its target lies under a
  declared data root (`site/` `resolve_and_open`).

## Connections

Each connection is one row of docs/integration.md, owned by its provider. Provides: `video.open`, `docs.open`,
`quarto.open`, `proxy.connect`, `ensure_proxy_for_url`, `pipe_one`, `record_browser_port`, `revoke_browser_ports`,
`is_servable_video`, `start_page_servers`, `remove_root`, `loopback_port_from_url`, `rust/protocol/src/page_url.rs`.
Uses: `LinkGate`, `proxy.connect`, `handle_connection`, `handle_proxy_connect`, `pipe_bidirectional`, `reject`,
`dispatch`, `ChildGuard`, `Signal`, `child_signal::fired`, `child_signal::process`, `sot_state_dir`, `sot_config_dir`,
`host_name`, `state_dir_hash`, `bound_pluto_port`, `allowed_proxy_ports`, `lane_dial`, `ResolvedDial`.

## Folders

- `site/`: the static-site server: grant tables, listeners, the request and the link rule.

Elsewhere: rust/frontend/src/pages.rs (the window's page proxy).

## Files

- `http.rs`: the response code both loopback servers share: content types, single ranges, file bodies, plain replies.
- `mod.rs`: declares the folder's modules and `start_page_servers`, which binds the listeners at boot.
  It also holds `random_token`, the one minter of video tokens, site nonces and pool secrets.
- `ops.rs`: video.open, docs.open (and its site-root walk), quarto.open.
- `proxy.rs`: `proxy.connect`, the loopback allowlist, browser-port records.
- `site/`: the static-site server (own page).
- `video.rs`: the video listener, its token grants and the request handler.

## Start here

`proxy.rs` `allowed_proxy_ports` for what a remote window may reach; `video.rs` `handle_conn` for a served request.
