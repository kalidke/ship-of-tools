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
- Minting fails closed: `random_token` returns `None` rather than a weak token.
- `proxy.connect` dials only 127.0.0.1 ports in `allowed_proxy_ports`, with a 5 s connect bound, and logs a refused
  port once per streak.
- A connection's sites go when it disconnects (`remove_root`).
- `.git` and `..` are never served, and a link is followed only when git tracks it and its target lies under a
  declared data root (`site/` `resolve_and_open`).

## Connections

- ops.rs serves `video.open`, `docs.open` and `quarto.open`; handlers.rs re-exports them.
- `server::run` binds the listeners at boot (`start_page_servers`) and hands a connection whose first frame is `proxy.connect` to
  `handle_proxy_connect`.
- lane_bridge.rs and lease.rs call `reject`, and lane_bridge.rs calls `pipe_bidirectional`, both through proxy.rs's re-export of server/pipe.rs.
- The REPL supervisor (repl.rs) records and revokes browser ports (`record_browser_port`, `revoke_browser_ports`).
- clients.rs calls `remove_root` when a connection disconnects.
- The window's page proxy (rust/frontend/src/proxy_listen.rs) dials `proxy.connect`.

## Folders

- `site/`: the static-site server: grant tables, listeners, the request and the link rule.

Elsewhere: rust/frontend/src/proxy_listen.rs (the window's page proxy).

## Files

- `http.rs`: the response code both loopback servers share: content types, single ranges, file bodies, plain replies.
- `mod.rs`: declares the folder's modules and `start_page_servers`, which binds the listeners at boot.
- `ops.rs`: video.open, docs.open (and its site-root walk), quarto.open.
- `proxy.rs`: `proxy.connect`, the loopback allowlist, browser-port records, and the re-export of `pipe_bidirectional` and `reject` (defined in server/pipe.rs).
- `site/`: the static-site server (own page).
- `video.rs`: the video listener, its token grants and the request handler.

## Start here

`proxy.rs` `allowed_proxy_ports` for what a remote window may reach; `video.rs` `handle_conn` for a served request.
