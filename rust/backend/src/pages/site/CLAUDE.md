# rust/backend/src/pages/site: the static-site server (pages)

The daemon serves an on-disk static site (typically an `index.html`) to the OS browser from loopback listeners, so
relative and root-relative links both resolve. `docs.open` grants a site to an owner (a window, or a connection of no
window); this folder holds the grant tables, the two kinds of listener and the request path, and the rule for what a request path may open. Part of the
daemon's page serving; charter: rust/backend/src/pages/CLAUDE.md.

## Files
- `mod.rs`: the grant tables (prefix nonces, pool ports, each by owner), the prefix and pool listeners, `handle_conn`
  for a connection's requests and `serve_request` for one.
- `links.rs`: `Site` and what a request path may open: the git link set, the data-roots file, the R0-R6 rules.
- `links_tests.rs`: tests of the follow rule over a real git repo and symlinked data roots (unix).
- `tests.rs`: tests of pool assignment, bind fallback, serving a prefix site and requests on a kept connection.

## Start here
`links.rs` `resolve_and_open` for what may be served; `mod.rs` `serve_request` for one request.

## Rules
- An owner is a window, kept across its redials, or a connection whose hello names no window (`ClientGuard::site_owner`).
- An owner holds one prefix site, named by a fresh nonce at every open; a re-open drops the old nonce, so a stale
  link 404s (`set_root`).
- An owner holds at most one pool port and every open mints a new secret; the first request authenticates by
  `?secret=` and gets an HttpOnly, SameSite=Strict cookie named for the pool listener, daemon host and port
  (`pool_cookie_name`: one browser can hold two daemons' sites at one pool port number, and a reopen overwrites its own
  cookie), and anything else is 403 (`serve_request`, secrets compared with `ct_eq`).
- Once `spawn_pool` ran, a pool port is assigned only from the ports it bound (`assign_pool_port`); when every one is
  held, the port of the window away longest is taken with a new secret (`Clients::away_sites`), never a live owner's.
- A request resolves by `resolve_and_open` in the order R0-R6: ordinary files only under the content root; a link only
  when git tracks it, a data root is declared, its target lies under one and the file stays inside it; the final open
  is `open_beneath`. Links are followed only on unix (R2d).
- The data-roots file and the git index are re-read only when their stat changes (`Site::data_roots`,
  `GitState::links`), and opening a site canonicalizes no data root.
- Resolution runs in `spawn_blocking` (`serve_request`); an owner's end (a connection of no window that disconnects, a
  window the roster forgets) drops its entries (`remove_root`).
- The pool is one table per process, so a unit test takes a pool port only while it holds `own_pool` (`tests.rs`),
  which empties the pool for it: the tests run in parallel and one counts all four ports.
