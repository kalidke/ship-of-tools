# ADR 0029: Multi-FE-correct `docs.open` site serving — per-connection site roots + disconnect cleanup

**Status:** Accepted (drafted, reviewed + signed off 2026-06-29 as faithful to all Q1-4 calls — bus `from-windows.md`/`from-linux.md`. FE launcher restart-race fix landed both OSes: `.ps1` `8980a55`, `.sh` mirror this commit)
**Date:** 2026-06-29

## Context

ADR 0024 gave us `docs.open` / the `W` keybind: a hand-rolled static HTTP
server (`site_serve.rs`) on loopback `:1236` (SSH-forwarded by the launcher),
serving any on-disk static site so relative *and* root-relative assets resolve,
opened in the OS browser. It shipped with a deliberately minimal model:

- **One process-wide site root** — `static SITE_ROOT: RwLock<Option<PathBuf>>`
  (site_serve.rs:37), re-pointed on every open ("last-opened wins", flagged in
  the source as a known single-user simplification).
- **One listener, bound once at startup** — `site_serve::spawn(1236)` at
  server.rs:324, whose bind failure is **non-fatal** (just `warn!("… W won't
  work")`).

Two problems surfaced in real use:

1. **Multi-FE clobber.** Per the daemon's multi-frontend model (one `sotd`, many
   connected FEs — a user roams several Windows FEs against one backend), two FEs
   opening different sites overwrite the single global root: FE-A's tab 404s on
   reload after FE-B opens. Two FEs cannot view different docs at once.
2. **No per-FE lifecycle / a silent outage.** Nothing ties the root (or the
   listener) to an FE connection; nothing cleans up when an FE disconnects. And
   the once-at-startup, non-fatal bind is what broke `W` for a whole session on
   2026-06-29: the launcher's `pkill -x sotd; sleep 0.3` restart race left the
   old `sotd` still holding `:1236`, the new `sotd`'s bind lost the race, logged
   the non-fatal warning, and ran **docs-less and undetected for ~45 min** until
   the user hit it on `MyPackage`.

**Empirical finding that shapes the fix:** the static sites we actually serve
(Documenter) are **root-relative-link-clean** — `MyPackage/docs/build/
index.html` has zero `href="/"`/`src="/"`; assets are page-relative
(`assets/…`, `search_index.js`, `../versions.js`) or absolute-external CDN.
`set_site_root` has exactly one caller (`handle_docs_open`); Pluto (`:1234`) and
video (`:1235`) run their own servers, not `site_serve`. So a **path-prefix**
scheme on the single `:1236` port serves today's content correctly *without*
touching the launcher's static `-L 1236` forward.

## Decision

Adopt **Option A — per-connection site roots, single port, path-prefixed URLs**,
with a loud guard for the one case it doesn't cover.

1. **Per-connection root map keyed by `ClientGuard.serial`.** Replace the global
   `SITE_ROOT` with `serial -> PathBuf`. The URL token *is* the serial.
   - Key by **`serial`** (monotonic per-connection, clients.rs:77), **not**
     `client_id` (the reconnect-stable id, ADR 0010). During an ADR-0010
     reconnect overlap the old half-open and new connections share a `client_id`
     but differ in `serial`; keying by `client_id` would let the old
     connection's teardown yank the entry the new one just created. Serial ⇒ one
     live connection owns exactly one map entry. Loopback-only, so an
     enumerable u64 token is fine (map to a nonce only if opacity is wanted —
     not bothering).
2. **`docs.open` returns `http://127.0.0.1:1236/<serial>/<rel>`;** `site_serve`
   strips the `<serial>` segment, looks up that connection's root, serves under
   it (traversal guard unchanged, now per-token root). The launcher and FE
   open-URL path are **unchanged** — the FE just opens the returned URL.
3. **Cleanup in `ClientGuard::drop`** (clients.rs:130-143), one line keyed by
   `self.serial`: `site_serve::remove_root(self.serial)`. **No separate ADR-0027
   reaper hook** — the reaper (keepalive ~50 s; write-timeout ≤10 s) is the
   mechanism that *guarantees* `Drop` is reached, so Drop-based cleanup inherits
   its coverage for free. The only path skipping `Drop` is a hard daemon crash,
   which tears down the whole process incl. the `:1236` listener and the
   in-memory map — nothing leaks. The map lives and dies with `sotd`.
4. **Thread connection context into the handler.** Dispatch passes only
   `(req_id, payload, &session)` today (the shared `Session`, not a per-conn id),
   but `client_guard` is in scope at the dispatch site (server.rs:845), with
   precedent (`handle_hello` gets `&clients`). Thread a small `ConnCtx { serial }`
   (or a bare `serial: u64`) into `handle_docs_open`.
5. **Loud root-relative guard (keep ADR-0024 honest).** ADR 0024 promised "any
   on-disk static site" incl. root-relative ones (an example project's `__site`, a genhtml
   coverage tree). Path-prefix `/<serial>/` breaks those (`/assets/x.css`
   escapes the token → 404). So `handle_docs_open` scans the resolved
   `index.html` for `href="/"`/`src="/"` and, if present, returns a clear error
   ("site uses root-relative links — shared-port serving not supported;
   port-pool is the deferred fix") **instead of serving a silently-broken page.**
6. **Bind-race fix — split + fold.**
   - *Split (FE, landed 2026-06-29):* the launcher replaces `pkill -x sotd;
     sleep 0.3` with pkill → wait-until-no-`sotd` (bounded, ~4.5 s) → SIGKILL any
     holdout, before starting the new `sotd` (launch-devenv.ps1; mirror in
     launch-devenv.sh). Pure ops hardening, independent of the map redesign.
   - *Fold (BE, part of this ADR):* make `site_serve::spawn`'s `:1236` bind
     **retry** briefly (a few attempts over ~2-3 s) so a transient port-still-held
     loses to a retry instead of disabling `W` for the whole session; optionally
     surface a failed bind to the FE as a health signal rather than a silent log.

### Why not Option B (static port pool) now

A per-FE *port* (distinct origin) is the only thing that makes **root-relative**
sites work for multiple FEs, but: (a) ADR 0024 established Win32-OpenSSH cannot
add forwards dynamically, so a pool must be **statically** forwarded at launch
(`-L 1236..1236+N`), capping concurrent FEs at N and requiring a launcher change
on every OS; (b) nothing we serve today is root-relative. So Option B is the
**deferred fallback**, to be built only when a real root-relative site appears
(the guard in #5 is what will tell us). Recorded here so the deferral is
explicit, not a silent narrowing of ADR 0024.

## Division of labor

- **BE:** `site_serve` serial→root map + prefix-strip + bind-retry;
  `docs.open` token URL + root-relative guard; `server.rs` thread `serial` into
  the handler; `ClientGuard::drop` cleanup line.
- **FE (win-fe):** launcher restart-race fix (**landed**); live cross-FE
  verification — two FEs viewing different Documenter sites at once, sub-pages +
  search working, and a disconnect dropping only the departing FE's entry.

## Consequences

- Two+ FEs can view different sites simultaneously; each FE's site state is
  reaped exactly when its connection drops, with no new teardown plumbing.
- The `W` outage class (silent docs-less session after a restart race) is closed
  from both ends: deterministic launcher restart + retrying, louder BE bind.
- ADR 0024's "any static site" generality is *temporarily* narrowed to
  page-relative sites, but **fails loud** for root-relative ones until Option B
  lands. Tracked as the trigger to build the port pool.


## Update 2026-07-03 — Option B IMPLEMENTED (per the maintainer: "make proper fix now")

The deferred per-port pool is in: root-relative entry HTML routes to a
dedicated port from `site_port()+1..=+POOL_SIZE` (1237-1240 by default) —
its own origin, served whole from its true root, so root-relative links
resolve. One pool site per connection (re-opens repoint it); ports are freed
by the same `ClientGuard::drop` hook as the prefix map; exhaustion returns a
clear `root_relative_pool_busy` error; per-port bind failures shrink the pool
non-fatally. The open-time guard is now the ROUTER between the two schemes,
not a refusal. All launchers forward the range statically. Found by, and
verified against, an example lab's project `__site` ("Shift+W isn't working").
