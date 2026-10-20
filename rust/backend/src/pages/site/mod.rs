// pages/site/mod.rs — a static HTTP/1.1 server for opening an on-disk static site
// or page (typically an `index.html`) in the OS browser with full fidelity.
//
// Sibling to `pages/video.rs` (the single-file, video-gated server). Where that
// one serves one explicit file path by extension, this serves a whole DIRECTORY
// TREE rooted at a CURRENT SITE ROOT that the `docs.open` handler sets per open:
// the site root it finds for the page (`find_site_root`). Serving from the true
// site root is what makes BOTH relative and root-relative (`/asset`) links resolve — `o`'s file://
// open can't (search/JS and root-relative assets break off-origin).
//
// One site per owner at a time (its last open wins): an owner is a window,
// kept across its redials, or a connection of no window (ADR 0029). Opening a
// second site re-points the owner's root, so a stale first tab 404s on reload;
// another owner's sites are untouched.
//
// Why hand-rolled rather than axum/tower-http: same reasoning as pages::video —
// the backend has no HTTP stack and the need is narrow (GET a file under a root).
// Range, content types and the empty-file case are shared with `pages::video`
// (`serve_file`), so a linked movie seeks the way a `video.open` one does.
//
// A `Site` splits two roots that used to be one. CONTENT root S is where
// ordinary files are served from (`find_site_root`). URL root is
// where the URL path space maps: the repo top when the page is opened from the
// shared prefix server and sits in a git repo, else S. A page's
// `../../data/x.mp4` can only reach the link folder, to be served or refused
// with words, when the URL space starts above S. Widening the URL space does not
// widen what is served: outside S, only files reached through a link git tracks
// are served, and only those under a mount folder (`/mnt`, `/media`,
// `/Volumes`), outside the home folder, with no hidden part below the mount
// folder, through a link to neither `/` nor a folder holding the home folder or
// the repository (`Bounds`; see `resolve_and_open` for the refusals). No file whose
// resolved path lies inside a `.git` directory below the site folder is served,
// and a linked file has no hidden part at all.
//
// Scope/security: binds 127.0.0.1 only, but neither port has auth of its own — any local user on a shared
// host can reach them once something is being served. Guards (security
// review): `docs.open` confines the servable root to a KNOWN workspace's
// project root (rejects anything canonicalizing outside every one of them,
// same check as `pluto.open`'s), and this file's traversal guard (below)
// rejects any per-request path resolving outside the CURRENT root once one is
// set. The shared `:1236` prefix server identifies a site by an unguessable
// per-open nonce (`set_root`), not the connection's raw serial, so URLs
// aren't enumerable. The `:1237-1240` pool ports can't use a path-prefix
// nonce (a root-relative site needs the whole path space), so THOSE get a
// per-open SECRET instead (`assign_pool_port`): the URL's `?secret=`
// authenticates the first request, which sets an HttpOnly cookie so later
// same-page asset fetches — no query string on those — authenticate via the
// cookie; anything else is a 403, not a silent serve.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
#[cfg(unix)]
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
#[cfg(test)]
use tokio::io::AsyncReadExt;
#[cfg(test)]
use tokio::io::AsyncWriteExt;
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::net::TcpStream;

use super::random_token;
use crate::pages::http::{content_type, read_request, serve_file, write_simple, Request};

/// Per-owner site roots, keyed by an unguessable per-open NONCE — not
/// the owner. (Security review: the old scheme put the raw,
/// small, monotonically-increasing connection serial in the URL, so any local
/// user could enumerate `/1/`, `/2/`, ... on this unauthenticated port and
/// view whatever site another user had open.) `docs.open` mints a fresh
/// nonce via `set_root` on every open; the returned URL carries it as the
/// first path segment (`/<nonce>/…`), so two FEs serve different sites on the
/// one `:1236` port without clobbering each other. An owner is a window, kept
/// across its redials, or a connection of no window (`ClientGuard::site_owner`).
/// `OWNER_NONCE` tracks which nonce belongs to which owner, so a re-open can
/// repoint (dropping the stale nonce's entry) and `ClientGuard::drop` can reap
/// the right one when its owner ends. Empty until the first open. `RwLock::new`
/// and `BTreeMap::new` are both const, so this initializes without lazy init.
static SITE_ROOTS: RwLock<BTreeMap<String, Arc<Site>>> = RwLock::new(BTreeMap::new());
static OWNER_NONCE: RwLock<BTreeMap<u64, String>> = RwLock::new(BTreeMap::new());

/// ADR 0029 Option B — the per-port pool for ROOT-RELATIVE sites. A site whose
/// entry HTML uses `/asset`-style links can't ride the shared `/{nonce}/`
/// prefix server (the link escapes the prefix and 404s), so it gets a DEDICATED
/// port from this small pool: its own origin, served from its true root, both
/// link styles resolve. `port -> (owner, site root, secret)`.
/// The `secret` is this security review's fix for the pool ports otherwise
/// having NO auth of their own (a path-prefix nonce can't work here — a
/// root-relative site needs the whole path space): `assign_pool_port` mints a
/// fresh one on every open, `docs.open` embeds it as `?secret=` in the
/// returned URL, and `serve_request`'s `ServeMode::Pool` arm requires it (or the
/// HttpOnly cookie it causes to be set) on every request. An owner holds
/// at most one pool port (re-opens repoint it AND mint a new secret,
/// invalidating the old one); the port is freed by `remove_root` when its owner
/// ends, same hook as the prefix map, or taken by a new open while its owner is
/// a window away (`assign_pool_port`). The range is fixed and contiguous
/// (`site_port()+1 ..= site_port()+POOL_SIZE`).
pub const POOL_SIZE: u16 = 4;
static POOL: RwLock<BTreeMap<u16, (u64, Arc<Site>, String)>> = RwLock::new(BTreeMap::new());

/// The pool ports THIS daemon successfully bound at `spawn_pool` — the ports
/// it actually serves. `assign_pool_port` picks only from here (not the full
/// configured range), so a port whose boot bind failed (another process owns
/// it) is never handed out in a docs URL and never authorized for the
/// ADR-0035 proxy. Empty until `spawn_pool` runs.
static BOUND_POOL: RwLock<std::collections::BTreeSet<u16>> =
    RwLock::new(std::collections::BTreeSet::new());
/// Whether `spawn_pool` has run. Distinguishes "never spawned" (an isolated
/// unit test exercising assignment → fall back to the full range) from
/// "spawned but bound nothing" (all binds failed → assign NOTHING, never a
/// failed/foreign port). Set true at spawn_pool entry regardless of outcome.
static POOL_SPAWNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pool's port range — 1237..=1240 with the default `site_port()` (1236).
pub fn pool_ports() -> std::ops::RangeInclusive<u16> {
    (site_port() + 1)..=(site_port() + POOL_SIZE)
}

/// Assign (or repoint) a pool port for `owner`, serving `root`,
/// and mint a fresh per-open secret for it. Reuses the owner's existing
/// port when it has one — one pool site per owner, matching the prefix
/// map's semantics — but ALWAYS mints a new secret, even on repoint, so a
/// stale secret (and the cookie it set) stops working the moment the site is
/// reopened. When every port is held, the port of the first owner in `away`
/// (the windows with no live connection, the one away longest first:
/// `Clients::away_sites`) is taken, with a new secret, so that window's old
/// link and cookie are refused. `away` runs under the pool's
/// lock, so no open can fall between its read and the take; the roster never
/// takes the pool's lock under its own. Returns `(port, secret)`; `None` if the
/// CSPRNG couldn't be read (fails closed rather than mint a guessable secret —
/// security review) or every port is held by an owner not away (the caller
/// surfaces "slots busy"; the two cases share one message since the RNG
/// failure should never actually happen).
pub fn assign_pool_port(owner: u64, site: Site, away: impl FnOnce() -> Vec<u64>) -> Option<(u16, String)> {
    let root = Arc::new(site);
    let secret = random_token()?;
    let mut g = POOL.write().unwrap_or_else(|p| p.into_inner());
    if let Some(port) = g
        .iter()
        .find(|(_, (s, _, _))| *s == owner)
        .map(|(&port, _)| port)
    {
        g.insert(port, (owner, root, secret.clone()));
        return Some((port, secret));
    }
    // Assign only from ports we ACTUALLY bound at spawn_pool — never a port
    // whose bind failed (another process owns it), which would otherwise
    // return a docs URL pointing at that unrelated listener AND authorize the
    // proxy to dial it (codex). Fall back to the full range ONLY when
    // spawn_pool never ran (an isolated unit test exercising assignment);
    // once spawned, BOUND_POOL is authoritative even when empty (all binds
    // failed → no candidates → None), never the full range (codex r3).
    let candidates: Vec<u16> = if POOL_SPAWNED.load(std::sync::atomic::Ordering::SeqCst) {
        BOUND_POOL
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .copied()
            .collect()
    } else {
        pool_ports().collect()
    };
    for &port in &candidates {
        if let std::collections::btree_map::Entry::Vacant(e) = g.entry(port) {
            e.insert((owner, root, secret.clone()));
            return Some((port, secret));
        }
    }
    let held_by = |o: &u64| candidates.iter().copied().find(|p| g.get(p).is_some_and(|(s, _, _)| s == o));
    let port = away().iter().find_map(held_by)?;
    g.insert(port, (owner, root, secret.clone()));
    Some((port, secret))
}

/// How many pool ports are currently owned (for the "slots busy" error).
pub fn pool_in_use() -> usize {
    POOL.read().unwrap_or_else(|p| p.into_inner()).len()
}

/// The pool ports currently ASSIGNED (and therefore actually bound + served
/// by THIS daemon) — the set the ADR-0035 proxy allowlist trusts. Distinct
/// from `pool_ports()` (the configured RANGE): a range port whose bind failed
/// at boot (another process owns it) is never assigned, so it never enters
/// this set and the proxy won't dial someone else's loopback service on it.
pub fn pool_assigned_ports() -> Vec<u16> {
    POOL.read()
        .unwrap_or_else(|p| p.into_inner())
        .keys()
        .copied()
        .collect()
}

/// Root AND secret for a pool port — what `serve_request`'s `ServeMode::Pool`
/// arm needs to authenticate a request (security review; see `assign_pool_port`).
fn pool_entry_for(port: u16) -> Option<(Arc<Site>, String)> {
    POOL.read()
        .unwrap_or_else(|p| p.into_inner())
        .get(&port)
        .map(|(_, r, s)| (r.clone(), s.clone()))
}

/// The env var keeps its historical `SOT_DOCS_PORT` name.
pub fn site_port() -> u16 {
    std::env::var("SOT_DOCS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1236)
}

/// The port the shared prefix server ACTUALLY bound (0 = not bound). Mirrors
/// `pages::video::BOUND_PORT`: `site_port()` is only a preference — on a shared
/// host another user's daemon may hold it, and `spawn` falls back to an
/// ephemeral port. `docs.open` URLs and the ADR-0035 proxy allowlist must
/// read this, never `site_port()`.
static BOUND_SITE_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

pub fn bound_site_port() -> Option<u16> {
    match BOUND_SITE_PORT.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        p => Some(p),
    }
}

/// Set the site root for `owner`, minting a fresh nonce and
/// returning it — the caller (`docs.open`) embeds it as the URL's first path
/// segment (`/<nonce>/…`). A repoint (this owner already has a live
/// nonce) drops the OLD nonce's `SITE_ROOTS` entry first, so a stale link a
/// browser tab still holds 404s instead of silently serving the new site.
/// `None` if the CSPRNG couldn't be read — fails closed rather than mint a
/// guessable nonce (security review). Ignores lock poisoning (a panicked
/// reader can't corrupt the map).
pub fn set_root(owner: u64, site: Site) -> Option<String> {
    let nonce = random_token()?;
    let mut sn = OWNER_NONCE.write().unwrap_or_else(|p| p.into_inner());
    let old_nonce = sn.insert(owner, nonce.clone());
    drop(sn);
    let mut g = SITE_ROOTS.write().unwrap_or_else(|p| p.into_inner());
    if let Some(old) = old_nonce {
        g.remove(&old);
    }
    g.insert(nonce.clone(), Arc::new(site));
    Some(nonce)
}

/// Drop `owner`'s site root AND free any pool port it holds.
/// Called from `ClientGuard::drop` when the owner ends: a connection of no
/// window that disconnects, or a window the roster forgets (ADR 0029, both
/// serving modes — one hook covers both).
pub fn remove_root(owner: u64) {
    let mut sn = OWNER_NONCE.write().unwrap_or_else(|p| p.into_inner());
    let nonce = sn.remove(&owner);
    drop(sn);
    if let Some(nonce) = nonce {
        SITE_ROOTS.write().unwrap_or_else(|p| p.into_inner()).remove(&nonce);
    }
    let mut p = POOL.write().unwrap_or_else(|p| p.into_inner());
    p.retain(|_, (s, _, _)| *s != owner);
}

fn root_for(nonce: &str) -> Option<Arc<Site>> {
    SITE_ROOTS
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(nonce)
        .cloned()
}

/// Constant-time byte comparison. Used for the pool-port secret/cookie check
/// below, so a timing side-channel can't help a local user guess it.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Percent-encode a relative URL path, keeping `/` as the separator and the
/// unreserved set verbatim; everything else (spaces, unicode, …) is `%XX`. Used
/// by the `docs.open` handler to build the open URL's path segment.
pub fn encode_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        match b {
            b'/' | b'-' | b'_' | b'.' | b'~' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Spawn the static-site server on `127.0.0.1:port`. Returns once the listener is
/// bound; the accept loop runs for the life of the process. Spawn once at
/// startup. No root is needed at spawn — `docs.open` sets it per open.
pub async fn spawn(preferred: u16) -> Result<()> {
    // Bind with a brief retry (ADR 0029). A `sotd` restart can race the previous
    // process's hold on this port; the launcher now waits for the old process to
    // exit first, but a transient TIME_WAIT / handover can still lose a single
    // bind. A few attempts over ~3s let a retry win instead of disabling `W` for
    // the whole session — the silent bind-failure outage class. If the preferred
    // port never frees (another USER's daemon owns it — the 2026-07-23 shared-host
    // multi-user collision, where all of 1236-1240 belonged to a second user and
    // `W` was dead for this one), fall back to an OS-assigned ephemeral port
    // instead of failing: `docs.open` URLs and the ADR-0035 proxy allowlist
    // read the ACTUAL port via `bound_site_port()`. Once bound, the accept
    // loop runs for the life of the process.
    const BIND_ATTEMPTS: u32 = 10;
    const BIND_RETRY_DELAY: Duration = Duration::from_millis(300);
    let mut bound = None;
    for attempt in 1..=BIND_ATTEMPTS {
        match crate::pages::bind_page_listener(preferred) {
            Ok(l) => {
                bound = Some(l);
                break;
            }
            Err(e) if attempt == BIND_ATTEMPTS => {
                tracing::warn!(preferred, error = %e, "static-site preferred port taken after {BIND_ATTEMPTS} attempts — falling back to an ephemeral port (multi-user host?)");
                bound = Some(
                    crate::pages::bind_page_listener(0).context(
                        "bind static-site server on an ephemeral 127.0.0.1 port",
                    )?,
                );
            }
            Err(e) => {
                tracing::warn!(preferred, attempt, error = %e, "static-site bind failed; retrying in 300ms");
                tokio::time::sleep(BIND_RETRY_DELAY).await;
            }
        }
    }
    let listener = bound.expect("bind loop breaks with Some or returns Err on the last attempt");
    let port = listener.local_addr().context("static-site server local_addr")?.port();
    BOUND_SITE_PORT.store(port, std::sync::atomic::Ordering::SeqCst);
    tracing::info!(port, "static-site server listening");
    tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "docs", |stream| async move {
        if let Err(e) = handle_conn(stream, ServeMode::Prefix).await {
            tracing::debug!(error = %e, "static-site conn ended");
        }
    }));
    Ok(())
}

/// Which serving scheme a listener speaks (ADR 0029).
#[derive(Clone, Copy)]
enum ServeMode {
    /// The shared `:1236` server: first path segment is the nonce `set_root`
    /// minted, selecting its owner's root; page-relative links only.
    Prefix,
    /// A pool port dedicated to one root-relative site: the whole request path
    /// resolves under the port's assigned root (its own origin).
    Pool(u16),
}

/// Spawn the Option-B pool listeners: `POOL_SIZE` of them, each serving
/// whichever root `assign_pool_port` has bound to it (404 until one is). The
/// `pool_ports()` range is only a PREFERENCE (stable);
/// a range port that's taken (another user's daemon on a shared host — the
/// 2026-07-23 shared-host collision took the entire 1237-1240 range) falls back to
/// an OS-assigned ephemeral port instead of shrinking the pool. Everything
/// downstream already keys on the ACTUAL bound ports: `BOUND_POOL` is what
/// `assign_pool_port` picks from, what `docs.open` URLs advertise, and what
/// `pool_assigned_ports()` feeds the ADR-0035 proxy allowlist.
pub async fn spawn_pool() {
    // Mark spawned BEFORE the binds so that even if all fail (BOUND_POOL stays
    // empty), assign_pool_port yields no candidates rather than falling back
    // to the full range (codex r3).
    POOL_SPAWNED.store(true, std::sync::atomic::Ordering::SeqCst);
    for preferred in pool_ports() {
        let listener = match crate::pages::bind_page_listener(preferred) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(preferred, error = %e, "pool preferred port taken — falling back to an ephemeral port (multi-user host?)");
                match crate::pages::bind_page_listener(0) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(error = %e, "pool ephemeral bind failed — pool shrinks by one");
                        continue;
                    }
                }
            }
        };
        let port = match listener.local_addr() {
            Ok(a) => a.port(),
            Err(e) => {
                tracing::warn!(error = %e, "pool listener local_addr failed — pool shrinks by one");
                continue;
            }
        };
        tracing::info!(port, "root-relative site pool listening");
        BOUND_POOL
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(port);
        tokio::spawn(sot_log::identity::peer_owner::serve_own(listener, "docs-pool", move |stream| async move {
            if let Err(e) = handle_conn(stream, ServeMode::Pool(port)).await {
                tracing::debug!(error = %e, port, "pool-site conn ended");
            }
        }));
    }
}

/// Minimal percent-decode for request-target paths (`%20` etc.). Good enough for
/// filesystem paths; not a general URL decoder.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The pool cookie's name for the pool listener at `port` on daemon host `host`. The browser's 127.0.0.1 cookies are
/// shared by every port and every daemon a window shows, and one browser can hold two sites at one pool port number
/// (two daemons' pools, reached through a remote window's page proxy at ports of its own; PAGE-PORT). So the name
/// identifies the listener: distinct for two daemons, the same across opens, so a reopen overwrites its own cookie and
/// the names stay bounded (at most one per pool port per daemon host). It takes no secret, so no open adds a name.
fn pool_cookie_name(host: &str, port: u16) -> String {
    if host.is_empty() {
        return format!("sot_pool_{port}");
    }
    let host: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    format!("sot_pool_{host}_{port}")
}

/// This daemon's host for `pool_cookie_name`, read once: `host_name()`, or empty when it fails (the name is then the
/// port's alone, as before PAGE-PORT).
fn pool_cookie_host() -> &'static str {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| sot_log::host::state_dir::host_name().unwrap_or_default())
}

/// Look up `name`'s value in a `Cookie:` header value (`name1=val1; name2=val2`).
/// Used by the pool-port auth check above.
fn cookie_value<'a>(cookie_hdr: &'a str, name: &str) -> Option<&'a str> {
    cookie_hdr.split(';').find_map(|kv| {
        let kv = kv.trim();
        kv.strip_prefix(name)?.strip_prefix('=')
    })
}

pub(crate) mod links;
use links::Site;
use links::resolve_and_open;

/// One accepted connection: its requests in turn (`read_request`), while each reply leaves it open.
async fn handle_conn(mut stream: TcpStream, mode: ServeMode) -> Result<()> {
    let mut buf = Vec::new();
    while let Some(request) = read_request(&mut stream, &mut buf).await? {
        if !serve_request(&mut stream, &request, mode).await? {
            break;
        }
    }
    Ok(())
}

/// The 405 for a method other than GET or HEAD.
async fn refuse_method(stream: &mut TcpStream, method: &str) -> Result<bool> {
    write_simple(
        stream,
        method,
        "405 Method Not Allowed",
        "only GET/HEAD",
        false,
    )
    .await
}

/// One request, from its target to its reply. Returns whether the connection stays open.
async fn serve_request(stream: &mut TcpStream, request: &Request, mode: ServeMode) -> Result<bool> {
    let (method, target, keep) = (request.method.as_str(), request.target.as_str(), request.keep);
    if method != "GET" && method != "HEAD" {
        return refuse_method(stream, method).await;
    }

    // Strip any query string / fragment, then resolve per serving mode
    // (ADR 0029): Prefix splits off the leading `/<nonce>/` segment to pick
    // its owner's root; a Pool port serves its assigned root whole, so
    // root-relative links resolve on that origin. `set_cookie`, if `Some`, is
    // a `Set-Cookie` header value the eventual 200 response must include.
    let raw_path = target.split(|c| c == '?' || c == '#').next().unwrap_or("");
    let (site, rest, set_cookie) = match mode {
        ServeMode::Pool(port) => {
            let Some((site, secret)) = pool_entry_for(port) else {
                return write_simple(
                    stream,
                    method,
                    "404 Not Found",
                    "no site assigned to this port (the daemon restarted or forgot the window that opened it)",
                    keep,
                )
                .await;
            };
            // Auth (security review): this port has no auth of its own, and
            // root-relative asset links can't ride a path-prefix nonce the
            // way the shared `:1236` server does (a root-relative site needs
            // the WHOLE path space). So the ONE-TIME secret `docs.open` put
            // in the URL's query string authenticates the FIRST request; that
            // response sets an HttpOnly cookie named for this pool listener
            // (`pool_cookie_name`) so every later same-page
            // asset fetch — which can't carry a query string — authenticates
            // via the cookie instead. A request that carries a `secret`
            // parameter is decided by it alone (the first one, when repeated,
            // and an empty one is refused; its key is matched after
            // percent-decoding and in any case, and a key with no `=` is
            // present and empty): a stale entry URL must not ride the port's
            // new cookie (#38). The cookie decides only a request with none.
            // The limit that stays: in-site navigation from an unreloaded
            // stale tab carries no parameter, so it rides the cookie. Neither
            // present or valid: 403.
            let cookie_name = pool_cookie_name(pool_cookie_host(), port);
            let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
            let query_secret = query.split('&').find_map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                percent_decode(k)
                    .eq_ignore_ascii_case("secret")
                    .then_some(v)
            });
            let cookie_ok = || {
                request
                    .cookie
                    .as_deref()
                    .and_then(|c| cookie_value(c, &cookie_name))
                    .map(|v| ct_eq(v.as_bytes(), secret.as_bytes()))
                    .unwrap_or(false)
            };
            let (ok, by_query) = match query_secret {
                Some(v) => (ct_eq(v.as_bytes(), secret.as_bytes()), true),
                None => (cookie_ok(), false),
            };
            if !ok {
                return write_simple(
                    stream,
                    method,
                    "403 Forbidden",
                    "missing or invalid site secret",
                    keep,
                )
                .await;
            }
            let set_cookie =
                by_query.then(|| format!("{cookie_name}={secret}; HttpOnly; SameSite=Strict; Path=/"));
            (site, raw_path.trim_start_matches('/').to_string(), set_cookie)
        }
        ServeMode::Prefix => {
            let trimmed = raw_path.trim_start_matches('/');
            let (token, rest) = match trimmed.split_once('/') {
                Some((t, r)) => (t, r),
                None => (trimmed, ""),
            };
            let Some(site) = root_for(token) else {
                // Either a stale/disconnected nonce, or a root-relative asset
                // (`/assets/…`) that landed here — `docs.open` routes sites
                // with root-relative links to the pool instead, so this is
                // belt-and-suspenders for that case too.
                return write_simple(
                    stream,
                    method,
                    "404 Not Found",
                    "no site open for this link (it was reopened, the daemon restarted or forgot the window \
                     that opened it, a `../` link on the page climbs above the top of the site, or this site \
                     uses root-relative links and should open via a pool port)",
                    keep,
                )
                .await;
            };
            (site, rest.to_string(), None)
        }
    };

    // Percent-decode the per-root path and resolve it. `resolve_and_open` does
    // blocking filesystem work (a stalled share must not stall the runtime).
    let decoded = percent_decode(&rest);
    let opened = {
        let site = site.clone();
        tokio::task::spawn_blocking(move || resolve_and_open(&site, &decoded)).await?
    };
    let (file, resolved) = match opened {
        Ok(f) => f,
        Err(r) => return write_simple(stream, method, r.status, &r.body, keep).await,
    };

    // no-cache so a rebuilt site is picked up without a hard browser refresh.
    let mut extra = String::from("Cache-Control: no-cache\r\n");
    if let Some(cookie) = &set_cookie {
        extra.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    serve_file(
        stream,
        request,
        tokio::fs::File::from_std(file),
        content_type(&resolved),
        &extra,
    )
    .await
}

#[cfg(test)]
mod pool_auth_tests;
#[cfg(test)]
mod tests;
