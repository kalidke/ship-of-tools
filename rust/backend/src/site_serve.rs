// site_serve.rs — a static HTTP/1.1 server for opening an on-disk static site
// or page (typically an `index.html`) in the OS browser with full fidelity.
//
// Sibling to `http_serve.rs` (the single-file, video-gated server). Where that
// one serves one explicit file path by extension, this serves a whole DIRECTORY
// TREE rooted at a CURRENT SITE ROOT that the `docs.open` handler sets per open:
// the cursored file's own directory. Serving from the true site root is what
// makes BOTH relative and root-relative (`/asset`) links resolve — `o`'s file://
// open can't (search/JS and root-relative assets break off-origin).
//
// One active site at a time (last-opened wins): each open re-points the root.
// Fine for a single user clicking around one site; opening a second site
// re-points the root, so a stale first tab would 404 on reload. A path-prefixed
// multi-site scheme is a later refinement (and root-relative links would still
// force one-site-per-origin anyway).
//
// Why hand-rolled rather than axum/tower-http: same reasoning as http_serve —
// the backend has no HTTP stack and the need is narrow (GET a file under a root).
// Range support is omitted: these assets are small and browsers don't seek them.
//
// Scope/security: binds 127.0.0.1 only (then SSH-forwarded, loopback on both
// ends), but neither port has auth of its own — any local user on a shared
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
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const READ_CHUNK: usize = 64 * 1024;

/// Per-connection site roots, keyed by an unguessable per-open NONCE — not
/// the connection serial. (Security review: the old scheme put the raw,
/// small, monotonically-increasing connection serial in the URL, so any local
/// user could enumerate `/1/`, `/2/`, ... on this unauthenticated port and
/// view whatever site another user had open.) `docs.open` mints a fresh
/// nonce via `set_root` on every open; the returned URL carries it as the
/// first path segment (`/<nonce>/…`), so two FEs serve different sites on the
/// one `:1236` port without clobbering each other. `SERIAL_NONCE` tracks which
/// nonce belongs to which live connection, so a re-open can repoint (dropping
/// the stale nonce's entry) and `ClientGuard::drop` can reap the right one on
/// disconnect. Empty until the first open. `RwLock::new` and `BTreeMap::new`
/// are both const, so this initializes without lazy init.
static SITE_ROOTS: RwLock<BTreeMap<String, PathBuf>> = RwLock::new(BTreeMap::new());
static SERIAL_NONCE: RwLock<BTreeMap<u64, String>> = RwLock::new(BTreeMap::new());

/// ADR 0029 Option B — the per-port pool for ROOT-RELATIVE sites. A site whose
/// entry HTML uses `/asset`-style links can't ride the shared `/{serial}/`
/// prefix server (the link escapes the prefix and 404s), so it gets a DEDICATED
/// port from this small pool: its own origin, served from its true root, both
/// link styles resolve. `port -> (owning connection serial, site root, secret)`.
/// The `secret` is this security review's fix for the pool ports otherwise
/// having NO auth of their own (a path-prefix nonce can't work here — a
/// root-relative site needs the whole path space): `assign_pool_port` mints a
/// fresh one on every open, `docs.open` embeds it as `?secret=` in the
/// returned URL, and `handle_conn`'s `ServeMode::Pool` arm requires it (or the
/// HttpOnly cookie it causes to be set) on every request. A connection holds
/// at most one pool port (re-opens repoint it AND mint a new secret,
/// invalidating the old one); the port is freed by `remove_root` on
/// disconnect, same hook as the prefix map. The range is fixed and contiguous
/// (`site_port()+1 ..= site_port()+POOL_SIZE`) so the launchers can
/// SSH-forward it statically.
pub const POOL_SIZE: u16 = 4;
static POOL: RwLock<BTreeMap<u16, (u64, PathBuf, String)>> = RwLock::new(BTreeMap::new());

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

/// Assign (or repoint) a pool port for connection `serial`, serving `root`,
/// and mint a fresh per-open secret for it. Reuses the connection's existing
/// port when it has one — one pool site per connection, matching the prefix
/// map's semantics — but ALWAYS mints a new secret, even on repoint, so a
/// stale secret (and the cookie it set) stops working the moment the site is
/// reopened. Returns `(port, secret)`; `None` if the CSPRNG couldn't be read
/// (fails closed rather than mint a guessable secret — security review) or
/// every port is owned by another live connection (the caller surfaces
/// "slots busy"; the two cases share one message since the RNG failure should
/// never actually happen).
pub fn assign_pool_port(serial: u64, root: PathBuf) -> Option<(u16, String)> {
    let secret = random_token()?;
    let mut g = POOL.write().unwrap_or_else(|p| p.into_inner());
    if let Some(port) = g
        .iter()
        .find(|(_, (s, _, _))| *s == serial)
        .map(|(&port, _)| port)
    {
        g.insert(port, (serial, root, secret.clone()));
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
    for port in candidates {
        if let std::collections::btree_map::Entry::Vacant(e) = g.entry(port) {
            e.insert((serial, root, secret.clone()));
            return Some((port, secret));
        }
    }
    None
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

/// Root AND secret for a pool port — what `handle_conn`'s `ServeMode::Pool`
/// arm needs to authenticate a request (security review; see `assign_pool_port`).
fn pool_entry_for(port: u16) -> Option<(PathBuf, String)> {
    POOL.read()
        .unwrap_or_else(|p| p.into_inner())
        .get(&port)
        .map(|(_, r, s)| (r.clone(), s.clone()))
}

/// Loopback port the static-site server listens on. Fixed (env-overridable) so
/// the launcher can SSH-forward it without negotiation. The env var keeps its
/// historical `SOT_DOCS_PORT` name so the existing launcher `-L` forward needs
/// no change. Resolved identically at spawn time and when building open URLs.
pub fn site_port() -> u16 {
    std::env::var("SOT_DOCS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1236)
}

/// Set the site root for connection `serial`, minting a fresh nonce and
/// returning it — the caller (`docs.open`) embeds it as the URL's first path
/// segment (`/<nonce>/…`). A repoint (this connection already has a live
/// nonce) drops the OLD nonce's `SITE_ROOTS` entry first, so a stale link a
/// browser tab still holds 404s instead of silently serving the new site.
/// `None` if the CSPRNG couldn't be read — fails closed rather than mint a
/// guessable nonce (security review). Ignores lock poisoning (a panicked
/// reader can't corrupt the map).
pub fn set_root(serial: u64, root: PathBuf) -> Option<String> {
    let nonce = random_token()?;
    let mut sn = SERIAL_NONCE.write().unwrap_or_else(|p| p.into_inner());
    let old_nonce = sn.insert(serial, nonce.clone());
    drop(sn);
    let mut g = SITE_ROOTS.write().unwrap_or_else(|p| p.into_inner());
    if let Some(old) = old_nonce {
        g.remove(&old);
    }
    g.insert(nonce.clone(), root);
    Some(nonce)
}

/// Drop connection `serial`'s site root AND free any pool port it owns.
/// Called from `ClientGuard::drop` so a disconnect reaps exactly the departing
/// connection's entries (ADR 0029, both serving modes — one hook covers both).
pub fn remove_root(serial: u64) {
    let mut sn = SERIAL_NONCE.write().unwrap_or_else(|p| p.into_inner());
    let nonce = sn.remove(&serial);
    drop(sn);
    if let Some(nonce) = nonce {
        SITE_ROOTS.write().unwrap_or_else(|p| p.into_inner()).remove(&nonce);
    }
    let mut p = POOL.write().unwrap_or_else(|p| p.into_inner());
    p.retain(|_, (s, _, _)| *s != serial);
}

fn root_for(nonce: &str) -> Option<PathBuf> {
    SITE_ROOTS
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .get(nonce)
        .cloned()
}

/// Generate an unguessable token (32 lowercase hex chars = 128 bits from the
/// OS CSPRNG), or `None` if the OS CSPRNG can't be read. Fails CLOSED
/// (security review): a predictable token defeats the whole point of this
/// scheme. Mirrors `http_serve`'s.
fn random_token() -> Option<String> {
    let mut buf = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut buf) {
        tracing::error!(error = %e, "random_token: OS CSPRNG read failed — refusing to mint a predictable token");
        return None;
    }
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Constant-time byte comparison — mirrors `handlers::constant_time_eq`
/// (duplicated rather than shared: this file already mirrors several small
/// helpers from its siblings, e.g. `random_token`, rather than reaching
/// across modules for them). Used for the pool-port secret/cookie check
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
pub async fn spawn(port: u16) -> Result<()> {
    // Bind with a brief retry (ADR 0029). A `sotd` restart can race the previous
    // process's hold on this port; the launcher now waits for the old process to
    // exit first, but a transient TIME_WAIT / handover can still lose a single
    // bind. A few attempts over ~3s let a retry win instead of disabling `W` for
    // the whole session — the silent bind-failure outage class. Once bound, the
    // accept loop runs for the life of the process.
    const BIND_ATTEMPTS: u32 = 10;
    const BIND_RETRY_DELAY: Duration = Duration::from_millis(300);
    let mut bound = None;
    for attempt in 1..=BIND_ATTEMPTS {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => {
                bound = Some(l);
                break;
            }
            Err(e) if attempt == BIND_ATTEMPTS => {
                return Err(e).with_context(|| {
                    format!("bind static-site server on 127.0.0.1:{port} after {BIND_ATTEMPTS} attempts")
                });
            }
            Err(e) => {
                tracing::warn!(port, attempt, error = %e, "static-site bind failed; retrying in 300ms");
                tokio::time::sleep(BIND_RETRY_DELAY).await;
            }
        }
    }
    let listener = bound.expect("bind loop breaks with Some or returns Err on the last attempt");
    tracing::info!(port, "static-site server listening");
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, ServeMode::Prefix).await {
                            tracing::debug!(error = %e, "static-site conn ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "static-site accept failed");
                }
            }
        }
    });
    Ok(())
}

/// Which serving scheme a listener speaks (ADR 0029).
#[derive(Clone, Copy)]
enum ServeMode {
    /// The shared `:1236` server: first path segment is the connection serial,
    /// selecting that connection's root; page-relative links only.
    Prefix,
    /// A pool port dedicated to one root-relative site: the whole request path
    /// resolves under the port's assigned root (its own origin).
    Pool(u16),
}

/// Spawn the Option-B pool listeners (`pool_ports()`), each serving whichever
/// root `assign_pool_port` has bound to it (404 until one is). Bind failures
/// are per-port and non-fatal: a lost port just shrinks the pool — `docs.open`
/// reports "slots busy" when none are assignable, never a silent break.
pub async fn spawn_pool() {
    // Mark spawned BEFORE the binds so that even if all fail (BOUND_POOL stays
    // empty), assign_pool_port yields no candidates rather than falling back
    // to the full range (codex r3).
    POOL_SPAWNED.store(true, std::sync::atomic::Ordering::SeqCst);
    for port in pool_ports() {
        match TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                tracing::info!(port, "root-relative site pool listening");
                BOUND_POOL
                    .write()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(port);
                tokio::spawn(async move {
                    loop {
                        match listener.accept().await {
                            Ok((stream, _peer)) => {
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        handle_conn(stream, ServeMode::Pool(port)).await
                                    {
                                        tracing::debug!(error = %e, port, "pool-site conn ended");
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, port, "pool-site accept failed");
                            }
                        }
                    }
                });
            }
            Err(e) => {
                tracing::warn!(port, error = %e, "pool port bind failed — pool shrinks by one");
            }
        }
    }
}

/// Content-Type for a static web asset by extension. Covers what a Documenter /
/// generic static site ships; falls back to octet-stream.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("wasm") => "application/wasm",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Minimal percent-decode for request-target paths (`%20` etc.). Good enough for
/// filesystem paths; not a general URL decoder. (Mirrors http_serve's.)
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

/// Look up `name`'s value in a `Cookie:` header value (`name1=val1; name2=val2`).
/// Used by the pool-port auth check above.
fn cookie_value<'a>(cookie_hdr: &'a str, name: &str) -> Option<&'a str> {
    cookie_hdr.split(';').find_map(|kv| {
        let kv = kv.trim();
        kv.strip_prefix(name)?.strip_prefix('=')
    })
}

/// Resolve a decoded request path to a real file under `root`, applying
/// directory-index (`/`, `/foo/`, or a directory → `index.html`) and the
/// traversal guard (the canonicalized target must stay within the canonicalized
/// root). Returns `None` for anything that doesn't resolve to a file inside root.
fn resolve(root: &Path, decoded: &str) -> Option<PathBuf> {
    let rel = decoded.trim_start_matches('/');
    let mut candidate = root.to_path_buf();
    if !rel.is_empty() {
        candidate.push(rel);
    }
    if rel.is_empty() || candidate.is_dir() {
        candidate.push("index.html");
    }
    let canon = candidate.canonicalize().ok()?;
    let root_canon = root.canonicalize().ok()?;
    if canon.starts_with(&root_canon) {
        Some(canon)
    } else {
        None
    }
}

async fn write_simple(stream: &mut TcpStream, status: &str, body: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

async fn handle_conn(mut stream: TcpStream, mode: ServeMode) -> Result<()> {
    // Read headers (up to the blank line), bounded so a malformed client can't
    // grow this unbounded.
    let mut buf = Vec::with_capacity(1024);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            return Ok(()); // client closed before a full request
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            return write_simple(&mut stream, "431 Request Header Fields Too Large", "headers too large").await;
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    if method != "GET" && method != "HEAD" {
        return write_simple(&mut stream, "405 Method Not Allowed", "only GET/HEAD").await;
    }

    // Cookie header (case-insensitive name) — only pool-mode auth needs it,
    // but it's cheap to always scan.
    let mut cookie_hdr: Option<String> = None;
    for line in lines {
        if let Some((name, val)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("cookie") {
                cookie_hdr = Some(val.trim().to_string());
            }
        }
    }

    // Strip any query string / fragment, then resolve per serving mode
    // (ADR 0029): Prefix splits off the leading `/<nonce>/` segment to pick
    // the connection's root; a Pool port serves its assigned root whole, so
    // root-relative links resolve on that origin. `set_cookie`, if `Some`, is
    // a `Set-Cookie` header value the eventual 200 response must include.
    let raw_path = target.split(|c| c == '?' || c == '#').next().unwrap_or("");
    let (root, rest, set_cookie) = match mode {
        ServeMode::Pool(port) => {
            let Some((root, secret)) = pool_entry_for(port) else {
                return write_simple(
                    &mut stream,
                    "404 Not Found",
                    "no site assigned to this port (its connection may have closed)",
                )
                .await;
            };
            // Auth (security review): this port has no auth of its own, and
            // root-relative asset links can't ride a path-prefix nonce the
            // way the shared `:1236` server does (a root-relative site needs
            // the WHOLE path space). So the ONE-TIME secret `docs.open` put
            // in the URL's query string authenticates the FIRST request; that
            // response sets an HttpOnly cookie (name includes the port —
            // cookies aren't port-scoped, so distinct sites on different pool
            // ports need distinct cookie names) so every later same-page
            // asset fetch — which can't carry a query string — authenticates
            // via the cookie instead. Neither present or valid: 403.
            let cookie_name = format!("sot_pool_secret_{port}");
            let cookie_ok = cookie_hdr
                .as_deref()
                .and_then(|c| cookie_value(c, &cookie_name))
                .map(|v| ct_eq(v.as_bytes(), secret.as_bytes()))
                .unwrap_or(false);
            let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
            let query_secret = query.split('&').find_map(|kv| {
                kv.split_once('=')
                    .filter(|(k, _)| *k == "secret")
                    .map(|(_, v)| v)
            });
            let query_ok = query_secret
                .map(|v| ct_eq(v.as_bytes(), secret.as_bytes()))
                .unwrap_or(false);
            if !cookie_ok && !query_ok {
                return write_simple(&mut stream, "403 Forbidden", "missing or invalid site secret").await;
            }
            let set_cookie = (!cookie_ok)
                .then(|| format!("{cookie_name}={secret}; HttpOnly; SameSite=Strict; Path=/"));
            (root, raw_path.trim_start_matches('/').to_string(), set_cookie)
        }
        ServeMode::Prefix => {
            let trimmed = raw_path.trim_start_matches('/');
            let (token, rest) = match trimmed.split_once('/') {
                Some((t, r)) => (t, r),
                None => (trimmed, ""),
            };
            let Some(root) = root_for(token) else {
                // Either a stale/disconnected nonce, or a root-relative asset
                // (`/assets/…`) that landed here — `docs.open` routes sites
                // with root-relative links to the pool instead, so this is
                // belt-and-suspenders for that case too.
                return write_simple(
                    &mut stream,
                    "404 Not Found",
                    "no site open for this link (it may have disconnected, or this \
                     site uses root-relative links and should open via a pool port)",
                )
                .await;
            };
            (root, rest.to_string(), None)
        }
    };

    // Percent-decode the per-root path and resolve under the chosen root.
    let decoded = percent_decode(&rest);
    let resolved = match resolve(&root, &decoded) {
        Some(p) => p,
        None => return write_simple(&mut stream, "404 Not Found", "no such file").await,
    };
    let meta = match tokio::fs::metadata(&resolved).await {
        Ok(m) if m.is_file() => m,
        _ => return write_simple(&mut stream, "404 Not Found", "no such file").await,
    };
    let total = meta.len();
    let ctype = content_type(&resolved);

    // no-cache so a rebuilt site is picked up without a hard browser refresh.
    let mut header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {total}\r\nCache-Control: no-cache\r\nConnection: close\r\n"
    );
    if let Some(cookie) = &set_cookie {
        header.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await?;

    if method == "HEAD" {
        stream.flush().await?;
        return Ok(());
    }

    let mut file = tokio::fs::File::open(&resolved).await?;
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let n = file.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        // A broken pipe just means the browser closed the connection — not an
        // error worth surfacing.
        if stream.write_all(&chunk[..n]).await.is_err() {
            return Ok(());
        }
    }
    stream.flush().await.ok();
    Ok(())
}

#[cfg(test)]
mod pool_tests {
    use super::*;

    fn reset() {
        POOL.write().unwrap_or_else(|p| p.into_inner()).clear();
    }

    #[test]
    fn assign_reuse_exhaust_free() {
        reset();
        let base = site_port();
        // Serials far outside anything other tests produce: clients.rs tests
        // drop ClientGuards with small serials, and each drop calls
        // remove_root(serial) — with colliding serials a parallel test run
        // freed this test's pool entries mid-assert.
        const S: u64 = 9_000_000_001;
        // Four distinct connections fill the pool in order.
        let (p1, s1) = assign_pool_port(S + 1, PathBuf::from("/a")).unwrap();
        let (p2, s2) = assign_pool_port(S + 2, PathBuf::from("/b")).unwrap();
        let (p3, _s3) = assign_pool_port(S + 3, PathBuf::from("/c")).unwrap();
        let (p4, _s4) = assign_pool_port(S + 4, PathBuf::from("/d")).unwrap();
        assert_eq!(
            vec![p1, p2, p3, p4],
            (base + 1..=base + POOL_SIZE).collect::<Vec<_>>()
        );
        // Distinct sites get distinct secrets.
        assert_ne!(s1, s2);
        // A re-open by an existing owner REPOINTS its port, not a new one,
        // and mints a FRESH secret — the old one (and any cookie it set)
        // stops working (security review).
        let (p2_again, s2_again) = assign_pool_port(S + 2, PathBuf::from("/b2")).unwrap();
        assert_eq!(p2_again, p2);
        assert_ne!(s2_again, s2);
        assert_eq!(
            pool_entry_for(p2).map(|(r, _)| r),
            Some(PathBuf::from("/b2"))
        );
        // Fifth connection: exhausted.
        assert_eq!(assign_pool_port(S + 5, PathBuf::from("/e")), None);
        assert_eq!(pool_in_use(), 4);
        // Disconnect frees exactly the owner's port; next claim gets it.
        remove_root(S + 3);
        assert_eq!(pool_in_use(), 3);
        let (p5, _s5) = assign_pool_port(S + 5, PathBuf::from("/e")).unwrap();
        assert_eq!(p5, p3);
        reset();
    }
}
