# rust/backend/src/pages/site: the static-site server (pages)

The daemon serves an on-disk static site (typically an `index.html`) to the OS browser from loopback listeners, so
relative and root-relative links both resolve. `docs.open` grants a site to an owner (a window, or a connection of no
window); this folder holds the grant tables, the two kinds of listener and the request path, and the rule for what a request path may open. Part of the
daemon's page serving; charter: rust/backend/src/pages/CLAUDE.md.

## Files
- `mod.rs`: the grant tables (prefix nonces, pool ports, each by owner), the prefix and pool listeners, `handle_conn`
  for a connection's requests and `serve_request` for one.
- `links.rs`: `Site` and what a request path may open: the git link set, the bounds of a link (`Bounds`), the R-rules.
- `links_tests.rs`: tests of the follow rule over a real git repo and a stand-in mount folder (unix).
- `links_case_tests.rs`: the folded home and repository comparison, and a folder index whose text leads out (unix).
- `pool_auth_tests.rs`: the pool listener's auth, one case per class of request.
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
  cookie), and anything else is 403 (`serve_request`, secrets compared with `ct_eq`). A request with a `secret`
  parameter is decided by it alone (the first, when repeated; the key matches after percent-decoding and in any case, and
  a key with no `=` is present and empty, which is refused); the cookie decides only a request with none, so a stale
  entry URL is refused, while in-site navigation from an unreloaded stale tab (no parameter) still rides the cookie.
- Once `spawn_pool` ran, a pool port is assigned only from the ports it bound (`assign_pool_port`); when every one is
  held, the port of the window away longest is taken with a new secret (`Clients::away_sites`), never a live owner's.
- A request resolves by `resolve_and_open`: ordinary files only under the content root; through a link only when git
  tracks it and the file's written path lies under a mount folder (`MOUNT_FOLDERS`), outside the home folder, with no
  hidden part below the mount folder (`Bounds`, fixed in code: no file lists data folders), and outside the
  repository (the home folder and the repository by folded names, `starts_with_folded`); the link's own target is judged first: as text, absolute plain names starting with a mount folder as
  written, before anything below the link is looked up; then equal to its canonical path (no symlink on its way),
  inside the same bounds and outside the repository. A tracked link serves exactly the files under its written target,
  reached by their written names, with no symlink anywhere below the link: the file is the target joined with the
  names asked, and the final open is `open_beneath` that mount folder, a symlink on the way answering as a missing
  file. Links are followed only on unix (R2d).
- Past the fast path no answer depends on whether a path exists: a path that reaches no link gets R2a, and a linked
  path that does not serve (target or file outside the bounds, missing, unreadable, not a regular file) gets R3; each
  names the path as asked.
- The git index is re-read only when its stat changes (`GitState::links`), and opening a site resolves no link
  target.
- Resolution runs in `spawn_blocking` (`serve_request`); an owner's end (a connection of no window that disconnects, a
  window the roster forgets) drops its entries (`remove_root`).
- The pool is one table per process, so a unit test takes a pool port only while it holds `own_pool` (`tests.rs`),
  which empties the pool for it: the tests run in parallel and one counts all four ports.
