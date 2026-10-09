# rust/backend/src/pages/site: the static-site server (pages)

The daemon serves an on-disk static site (typically an `index.html`) to the OS browser from loopback listeners, so
relative and root-relative links both resolve. `docs.open` grants a site to a connection; this folder holds the grant
tables, the two kinds of listener and the request path, and the rule for what a request path may open. Part of the
daemon's page serving; charter: rust/backend/src/pages/CLAUDE.md.

## Files
- `mod.rs`: the grant tables (prefix nonces, pool ports), the prefix and pool listeners, and `handle_conn` for one request.
- `links.rs`: `Site` and what a request path may open: the git link set, the data-roots file, the R0-R6 rules.
- `links_tests.rs`: tests of the follow rule over a real git repo and symlinked data roots (unix).
- `tests.rs`: tests of pool assignment, bind fallback and serving a prefix site.

## Start here
`links.rs` `resolve_and_open` for what may be served; `mod.rs` `handle_conn` for one request.

## Rules
- A connection holds one prefix site, named by a fresh nonce at every open; a re-open drops the old nonce, so a stale
  link 404s (`set_root`).
- A connection holds at most one pool port and every open mints a new secret; the first request authenticates by
  `?secret=` and gets an HttpOnly, SameSite=Strict cookie named for the pool listener, daemon host and port
  (`pool_cookie_name`: one browser can hold two daemons' sites at one pool port number, and a reopen overwrites its own
  cookie), and anything else is 403 (`handle_conn`, secrets compared with `ct_eq`).
- Once `spawn_pool` ran, a pool port is assigned only from the ports it bound (`assign_pool_port`).
- A request resolves by `resolve_and_open` in the order R0-R6: ordinary files only under the content root; a link only
  when git tracks it, a data root is declared, its target lies under one and the file stays inside it; the final open
  is `open_beneath`. Links are followed only on unix (R2d).
- The data-roots file and the git index are re-read only when their stat changes (`Site::data_roots`,
  `GitState::links`), and opening a site canonicalizes no data root.
- Resolution runs in `spawn_blocking` (`handle_conn`); a disconnect drops the connection's entries (`remove_root`).
