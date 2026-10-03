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
// Range, content types and the empty-file case are shared with `http_serve`
// (`serve_file`), so a linked movie seeks the way a `video.open` one does.
//
// A `Site` splits two roots that used to be one. CONTENT root S is where
// ordinary files are served from (`find_site_root`, unchanged). URL root is
// where the URL path space maps: the repo top when the page is opened from the
// shared prefix server, git tracks a symlink under it and this machine declares
// a data root, else S. A page's `../../data/x.mp4` can only reach the link
// folder when the URL space starts above S. Widening the URL space does not
// widen what is served: outside S, only files inside the target of a link git
// tracks, under a data root the machine's own `data-roots` file declares, are
// served (see `resolve_and_open` for the refusals). No file whose resolved path
// lies inside a `.git` directory below the folder it is served from (the site
// or the data root) is served.
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
#[cfg(unix)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
#[cfg(unix)]
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
#[cfg(test)]
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::http_serve::{content_type, serve_file, write_simple};

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
static SITE_ROOTS: RwLock<BTreeMap<String, Arc<Site>>> = RwLock::new(BTreeMap::new());
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
pub fn assign_pool_port(serial: u64, site: Site) -> Option<(u16, String)> {
    let root = Arc::new(site);
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
fn pool_entry_for(port: u16) -> Option<(Arc<Site>, String)> {
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

/// The port the shared prefix server ACTUALLY bound (0 = not bound). Mirrors
/// `http_serve::BOUND_PORT`: `site_port()` is only a preference — on a shared
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

/// Set the site root for connection `serial`, minting a fresh nonce and
/// returning it — the caller (`docs.open`) embeds it as the URL's first path
/// segment (`/<nonce>/…`). A repoint (this connection already has a live
/// nonce) drops the OLD nonce's `SITE_ROOTS` entry first, so a stale link a
/// browser tab still holds 404s instead of silently serving the new site.
/// `None` if the CSPRNG couldn't be read — fails closed rather than mint a
/// guessable nonce (security review). Ignores lock poisoning (a panicked
/// reader can't corrupt the map).
pub fn set_root(serial: u64, site: Site) -> Option<String> {
    let nonce = random_token()?;
    let mut sn = SERIAL_NONCE.write().unwrap_or_else(|p| p.into_inner());
    let old_nonce = sn.insert(serial, nonce.clone());
    drop(sn);
    let mut g = SITE_ROOTS.write().unwrap_or_else(|p| p.into_inner());
    if let Some(old) = old_nonce {
        g.remove(&old);
    }
    g.insert(nonce.clone(), Arc::new(site));
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

fn root_for(nonce: &str) -> Option<Arc<Site>> {
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
        match TcpListener::bind(("127.0.0.1", preferred)).await {
            Ok(l) => {
                bound = Some(l);
                break;
            }
            Err(e) if attempt == BIND_ATTEMPTS => {
                tracing::warn!(preferred, error = %e, "static-site preferred port taken after {BIND_ATTEMPTS} attempts — falling back to an ephemeral port (multi-user host?)");
                bound = Some(
                    TcpListener::bind(("127.0.0.1", 0)).await.context(
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

/// Spawn the Option-B pool listeners: `POOL_SIZE` of them, each serving
/// whichever root `assign_pool_port` has bound to it (404 until one is). The
/// `pool_ports()` range is only a PREFERENCE (stable, launcher-forwardable);
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
        let listener = match TcpListener::bind(("127.0.0.1", preferred)).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(preferred, error = %e, "pool preferred port taken — falling back to an ephemeral port (multi-user host?)");
                match TcpListener::bind(("127.0.0.1", 0)).await {
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
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _peer)) => {
                        tokio::spawn(async move {
                            if let Err(e) = handle_conn(stream, ServeMode::Pool(port)).await {
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

/// One open site: where ordinary files are served from, where the URL space
/// maps, and (unix, when this machine declares data roots and the folder is in
/// a git repo) the state needed to follow tracked links. See the header.
pub struct Site {
    /// Canonical S: the only place ordinary files are served from.
    content_root: PathBuf,
    /// Canonical root of the URL path space: S, or the repo top when widened.
    url_root: PathBuf,
    /// Test-only override of the data-roots file; `None` reads the config dir.
    roots_file: Option<PathBuf>,
    /// The canonical data roots, keyed on the data-roots file's stat: an
    /// unchanged file is never canonicalized again (a stalled share would
    /// otherwise stall every request). A miss still canonicalizes every root,
    /// so a dead hard mount stalls that request and the ones queued behind it.
    #[cfg(unix)]
    roots_cache: Mutex<Option<(Option<IndexStamp>, std::sync::Arc<DataRoots>)>>,
    /// Test-only: how many times the file was read and its roots canonicalized.
    #[cfg(all(unix, test))]
    roots_reads: std::sync::atomic::AtomicUsize,
    #[cfg(unix)]
    git: Option<GitState>,
}

/// The repo a site sits in, and the tracked-link set built from its index.
#[cfg(unix)]
struct GitState {
    top: PathBuf,
    index: PathBuf,
    cache: Mutex<LinkCache>,
}

#[cfg(unix)]
#[derive(Default)]
struct LinkCache {
    stamp: Option<IndexStamp>,
    /// Tracked symlinks (git mode 120000), relative to the site's `url_root`.
    links: BTreeSet<PathBuf>,
}

/// mtime, size and inode of the repo's index file: any `git add`/`git rm`
/// rewrites it (by rename), so a changed stamp means the link set is stale.
#[cfg(unix)]
type IndexStamp = (std::time::SystemTime, u64, u64);

#[cfg(unix)]
fn index_stamp(index: &Path) -> Option<IndexStamp> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(index).ok()?;
    Some((m.modified().ok()?, m.len(), m.ino()))
}

/// The declared data roots as read from the file, canonical, plus every line
/// that was skipped and why (named in the R2c/R3 refusal so a person sees why
/// their root did not count).
#[cfg(unix)]
#[derive(Debug, Default, Clone)]
pub(crate) struct DataRoots {
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) skipped: Vec<String>,
}

/// Read the data-roots file: one absolute directory per line, blank and `#`
/// lines ignored. A line is skipped (fail closed) when it is relative, does not
/// canonicalize (not on this host), is not a directory, or is `/` or contains
/// the canonical `home`: that would make a tracked link to the user's ssh
/// directory followable. A missing file declares nothing.
#[cfg(unix)]
pub(crate) fn read_data_roots(file: &Path, home: Option<&Path>) -> DataRoots {
    let mut out = DataRoots::default();
    let Ok(text) = std::fs::read_to_string(file) else {
        return out;
    };
    let home = home.and_then(|h| h.canonicalize().ok());
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let p = Path::new(line);
        if !p.is_absolute() {
            tracing::warn!(root = line, "data-roots: relative path skipped");
            out.skipped.push(format!("{line} (not an absolute path)"));
            continue;
        }
        let Ok(canon) = p.canonicalize() else {
            tracing::debug!(root = line, "data-roots: not on this host, skipped");
            out.skipped.push(format!("{line} (does not exist on this host)"));
            continue;
        };
        if !canon.is_dir() {
            out.skipped.push(format!("{line} (not a directory)"));
            continue;
        }
        if canon.parent().is_none() || home.as_ref().is_some_and(|h| h.starts_with(&canon)) {
            tracing::warn!(root = line, "data-roots: root contains the home directory, skipped");
            out.skipped.push(format!("{line} (contains the home directory)"));
            continue;
        }
        out.roots.push(canon);
    }
    out
}

/// Run `git` in `dir` with the repo-selecting env removed, stdin null, and a
/// 10 s limit. `Err(true)` = git ran and exited nonzero (not a repo, or a
/// refusal, first stderr line included); `Err(false)` = it could not run or
/// timed out.
#[cfg(unix)]
fn run_git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, (bool, String)> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, held) = crate::shutdown::process().spawn_std(&mut cmd).map_err(|e| (false, e.to_string()))?;
    let mut so = child.stdout.take().expect("piped");
    let mut se = child.stderr.take().expect("piped");
    let t_out = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = so.read_to_end(&mut v);
        v
    });
    let t_err = std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = se.read_to_end(&mut v);
        v
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match crate::contain::exited(&mut child, false) {
            Ok(true) => break,
            Ok(false) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(false) => {
                drop(held);
                let _ = child.kill();
                let _ = child.wait();
                return Err((false, "timed out after 10 s".into()));
            }
            Err(e) => {
                drop(held);
                let _ = child.kill();
                let _ = child.wait();
                return Err((false, e.to_string()));
            }
        }
    }
    // Whatever git started dies with it, before the pipes are joined and the child is reaped.
    drop(held);
    let status = child.wait().map_err(|e| (false, e.to_string()))?;
    let out = t_out.join().unwrap_or_default();
    let err = t_err.join().unwrap_or_default();
    if status.success() {
        Ok(out)
    } else {
        let first = String::from_utf8_lossy(&err).lines().next().unwrap_or("").to_string();
        Err((true, first))
    }
}

#[cfg(unix)]
impl GitState {
    /// The repo containing `dir`: its canonical top and index file. `None`
    /// (fail closed, no links) when it is not a repo or git will not answer.
    fn find(dir: &Path) -> Option<GitState> {
        let out = match run_git(dir, &["rev-parse", "--show-toplevel", "--git-path", "index"]) {
            Ok(o) => o,
            Err((true, first)) if first.is_empty() || first.contains("not a git repository") => {
                tracing::debug!(dir = %dir.display(), "site_serve: not a git repo, no links");
                return None;
            }
            Err((_, first)) => {
                tracing::warn!(dir = %dir.display(), error = first, "site_serve: git refused, no links followed");
                return None;
            }
        };
        let text = String::from_utf8_lossy(&out).into_owned();
        let mut lines = text.lines();
        let top = PathBuf::from(lines.next()?).canonicalize().ok()?;
        let index = dir.join(lines.next()?);
        Some(GitState { top, index, cache: Mutex::new(LinkCache::default()) })
    }

    /// Tracked symlinks under `url_root`, relative to it. Re-stats the index on
    /// every call and rebuilds the set only when the stamp changed, so a link
    /// untracked (or added) after the open is honoured at the next request.
    /// The stamp is taken BEFORE listing, so a write during the listing shows
    /// as changed next time.
    fn links(&self, url_root: &Path) -> BTreeSet<PathBuf> {
        let stamp = index_stamp(&self.index);
        let mut c = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if stamp.is_none() || c.stamp != stamp {
            c.links = self.list_links(url_root);
            c.stamp = stamp;
        }
        c.links.clone()
    }

    fn list_links(&self, url_root: &Path) -> BTreeSet<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let mut set = BTreeSet::new();
        let out = match run_git(&self.top, &["ls-files", "-s", "-z"]) {
            Ok(o) => o,
            Err((_, first)) => {
                tracing::warn!(error = first, "site_serve: git ls-files failed, no links followed");
                return set;
            }
        };
        for entry in out.split(|&b| b == 0) {
            let Some(tab) = entry.iter().position(|&b| b == b'\t') else { continue };
            if !entry.starts_with(b"120000 ") {
                continue;
            }
            let rel = Path::new(std::ffi::OsStr::from_bytes(&entry[tab + 1..]));
            if let Ok(under) = self.top.join(rel).strip_prefix(url_root) {
                set.insert(under.to_path_buf());
            }
        }
        set
    }
}

impl Site {
    /// A plain site: served from `root` alone, no linked data. Today's behaviour.
    pub fn plain(root: PathBuf) -> Site {
        let root = root.canonicalize().unwrap_or(root);
        Site {
            content_root: root.clone(),
            url_root: root,
            roots_file: None,
            #[cfg(unix)]
            roots_cache: Mutex::new(None),
            #[cfg(all(unix, test))]
            roots_reads: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(unix)]
            git: None,
        }
    }

    /// Open a site for `docs.open`, reading the machine's data-roots file.
    /// `ws_root` bounds how far the URL space may widen; `widen` is false for
    /// root-relative sites (they keep S as their URL root).
    pub async fn open(content_root: PathBuf, ws_root: PathBuf, widen: bool) -> Site {
        Self::build(content_root, ws_root, widen, None).await
    }

    /// As `open`, naming the data-roots file (tests never read the real config).
    #[cfg(test)]
    pub(crate) async fn with_roots(
        content_root: PathBuf,
        ws_root: PathBuf,
        widen: bool,
        roots_file: PathBuf,
    ) -> Site {
        Self::build(content_root, ws_root, widen, Some(roots_file)).await
    }

    async fn build(
        content_root: PathBuf,
        ws_root: PathBuf,
        widen: bool,
        roots_file: Option<PathBuf>,
    ) -> Site {
        let mut site = Site::plain(content_root);
        site.roots_file = roots_file;
        #[cfg(unix)]
        {
            // git is only asked when the machine declares at least one root;
            // no root is canonicalized here, a dead share must not stall open.
            let site = tokio::task::spawn_blocking(move || {
                if site.declares_a_root() {
                    site.attach_git(&ws_root, widen);
                }
                site
            })
            .await;
            return site.expect("site build task");
        }
        #[cfg(not(unix))]
        {
            let _ = (ws_root, widen);
            site
        }
    }

    #[cfg(unix)]
    fn attach_git(&mut self, ws_root: &Path, widen: bool) {
        let Some(mut g) = GitState::find(&self.content_root) else { return };
        if widen {
            let ws = ws_root.canonicalize().unwrap_or_else(|_| ws_root.to_path_buf());
            // The inner of the repo top and the workspace root; both are
            // ancestors of S, so one contains the other.
            let inner = if g.top.starts_with(&ws) {
                Some(g.top.clone())
            } else if ws.starts_with(&g.top) {
                Some(ws)
            } else {
                None
            };
            if let Some(r) = inner {
                if self.content_root.starts_with(&r)
                    && r != self.content_root
                    && !g.links(&r).is_empty()
                {
                    self.url_root = r;
                }
            }
            // The probe above listed links relative to the candidate root;
            // start clean so the set is always relative to the final one.
            g.cache = Mutex::new(LinkCache::default());
        }
        self.git = Some(g);
    }

    #[cfg(unix)]
    fn roots_path(&self) -> Option<PathBuf> {
        self.roots_file
            .clone()
            .or_else(|| sot_log::state_dir::sot_config_dir().map(|d| d.join("data-roots")))
    }

    /// Whether the data-roots file has a non-comment absolute line: text only,
    /// nothing is resolved.
    #[cfg(unix)]
    fn declares_a_root(&self) -> bool {
        let Some(text) = self.roots_path().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return false;
        };
        text.lines().map(str::trim).any(|l| !l.starts_with('#') && Path::new(l).is_absolute())
    }

    /// The data roots as of the file's current stat: a root added or removed
    /// is seen at the next request, an unchanged file is not re-read. No
    /// config dir (`None`) means no roots.
    #[cfg(unix)]
    fn data_roots(&self) -> std::sync::Arc<DataRoots> {
        let Some(f) = self.roots_path() else {
            return Default::default();
        };
        let stamp = index_stamp(&f);
        let mut cache = self.roots_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((s, r)) = cache.as_ref() {
            if *s == stamp {
                return r.clone();
            }
        }
        #[cfg(test)]
        self.roots_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let r = std::sync::Arc::new(read_data_roots(
            &f,
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        ));
        *cache = Some((stamp, r.clone()));
        r
    }

    /// The URL path (below the nonce) of a page `rel` under the content root:
    /// `rel` itself when the URL space is not widened.
    pub fn url_path(&self, rel: &str) -> String {
        match self.content_root.strip_prefix(&self.url_root) {
            Ok(s) if !s.as_os_str().is_empty() => {
                let s = s.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
                format!("{s}/{rel}")
            }
            _ => rel.to_string(),
        }
    }
}

/// Why a request was not served, and the reply to send.
pub(crate) struct Refusal {
    status: &'static str,
    body: String,
}

fn refuse(path: &str, status: &'static str, code: &str, body: String) -> Refusal {
    tracing::info!(path, reason = code, "site_serve: refused");
    Refusal { status, body }
}

fn not_found() -> Refusal {
    Refusal { status: "404 Not Found", body: "no such file".into() }
}

/// Open `rel` beneath `base` without following a symlink anywhere below it: `base`
/// by its canonical path, each intermediate component `O_DIRECTORY|O_NOFOLLOW`,
/// the last `O_NOFOLLOW|O_NONBLOCK` (a FIFO cannot hang the task). A symlink
/// swapped in after the caller's check fails with ELOOP or ENOTDIR.
#[cfg(unix)]
fn open_beneath(base: &Path, rel: &Path) -> std::io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::io::{Error, ErrorKind};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    let cstr = |b: &[u8]| CString::new(b).map_err(|_| Error::from(ErrorKind::InvalidInput));
    let comps: Vec<&std::ffi::OsStr> = rel
        .components()
        .map(|c| match c {
            std::path::Component::Normal(n) => Ok(n),
            _ => Err(Error::from(ErrorKind::InvalidInput)),
        })
        .collect::<Result<_, _>>()?;
    if comps.is_empty() {
        return Err(Error::from(ErrorKind::InvalidInput));
    }
    let flags = libc::O_RDONLY | libc::O_CLOEXEC;
    let cbase = cstr(base.as_os_str().as_bytes())?;
    // SAFETY: plain open(2) on a NUL-terminated path; the fd is owned at once.
    let fd = unsafe { libc::open(cbase.as_ptr(), flags | libc::O_DIRECTORY) };
    if fd < 0 {
        return Err(Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh, valid descriptor nobody else owns.
    let mut dir = unsafe { OwnedFd::from_raw_fd(fd) };
    let last = comps.len() - 1;
    for (i, c) in comps.iter().enumerate() {
        let cc = cstr(c.as_bytes())?;
        let f = if i == last {
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            flags | libc::O_DIRECTORY | libc::O_NOFOLLOW
        };
        // SAFETY: openat(2) relative to a live directory fd, NUL-terminated name.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), cc.as_ptr(), f) };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        // SAFETY: as above, a fresh descriptor.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        if i == last {
            return Ok(std::fs::File::from(owned));
        }
        dir = owned;
    }
    unreachable!("comps is non-empty and the last iteration returns")
}

#[cfg(not(unix))]
fn open_beneath(base: &Path, rel: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(base.join(rel))
}

/// `open_beneath`, mapping the outcome to a file or a refusal: a swapped-in
/// symlink is R6, anything else that is not a file is a 404.
fn open_checked(base: &Path, rel: &Path, shown: &str) -> Result<std::fs::File, Refusal> {
    #[cfg(not(unix))]
    let _ = shown;
    // R0 on the resolved path: a link into `.git`, an 8.3 short name or an
    // NTFS stream all canonicalize to a `.git` component.
    if rel.components().any(|c| c.as_os_str().eq_ignore_ascii_case(".git")) {
        return Err(refuse(
            shown,
            "403 Forbidden",
            "R0",
            "refused: .git is never served".into(),
        ));
    }
    match open_beneath(base, rel) {
        Ok(f) => Ok(f),
        #[cfg(unix)]
        Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP) | Some(libc::ENOTDIR)) => Err(refuse(
            shown,
            "403 Forbidden",
            "R6",
            format!("refused: {shown} changed while it was being opened"),
        )),
        Err(_) => Err(not_found()),
    }
}

/// Resolve a decoded request path and open the file, applying the follow rule
/// in order. Runs in `spawn_blocking`. Returns the open file and the canonical
/// path its content type comes from.
///
/// R0 `.git` anywhere; R1 a `..` part; then Q = url_root + parts (a directory
/// gets `index.html`). Fast path: Q canonicalizes inside S. Slow path: the
/// first symlink below url_root must be a link git tracks (R2b), a data root
/// must be declared (R2c), the link's target must lie under one (R3), it must
/// resolve (R5), the file must stay inside the target (R4), and the open is
/// `open_beneath` the data root (R6). Not unix: no link is ever followed (R2d).
pub(crate) fn resolve_and_open(
    site: &Site,
    decoded: &str,
) -> Result<(std::fs::File, PathBuf), Refusal> {
    let mut parts: Vec<&str> = Vec::new();
    for p in decoded.split(|c| c == '/' || c == '\\') {
        match p {
            "" | "." => {}
            ".." => {
                return Err(refuse(
                    decoded,
                    "403 Forbidden",
                    "R1",
                    "refused: the request path contains a \"..\" segment".into(),
                ))
            }
            _ if p.eq_ignore_ascii_case(".git") => {
                return Err(refuse(
                    decoded,
                    "403 Forbidden",
                    "R0",
                    "refused: .git is never served".into(),
                ))
            }
            _ => parts.push(p),
        }
    }
    let mut q = site.url_root.clone();
    for p in &parts {
        q.push(p);
    }
    if parts.is_empty() || q.is_dir() {
        q.push("index.html");
        parts.push("index.html");
    }
    let shown = parts.join("/");

    // Fast path: today's rule, confined to the content root.
    if let Ok(f) = q.canonicalize() {
        if let Ok(rel) = f.strip_prefix(&site.content_root) {
            let file = open_checked(&site.content_root, rel, &shown)?;
            return Ok((file, f));
        }
    }

    // Slow path: find the first symlink below url_root.
    let mut cur = site.url_root.clone();
    let mut link = PathBuf::new();
    let mut found = false;
    for p in &parts {
        cur.push(p);
        link.push(p);
        match std::fs::symlink_metadata(&cur) {
            Err(_) => return Err(not_found()),
            Ok(m) if m.file_type().is_symlink() => {
                found = true;
                break;
            }
            Ok(_) => {}
        }
    }
    if !found {
        return Err(refuse(
            &shown,
            "403 Forbidden",
            "R2a",
            format!("refused: {shown} is outside the site folder; only the site folder and the repo's tracked data links are served"),
        ));
    }
    let l = link.to_string_lossy().into_owned();
    #[cfg(not(unix))]
    {
        let _ = &q;
        return Err(refuse(
            &shown,
            "403 Forbidden",
            "R2d",
            format!("refused: {l} is a link; linked data is followed only on Linux and macOS"),
        ));
    }
    #[cfg(unix)]
    follow_link(site, &q, &shown, &link, &l)
}

#[cfg(unix)]
fn follow_link(
    site: &Site,
    q: &Path,
    shown: &str,
    link: &Path,
    l: &str,
) -> Result<(std::fs::File, PathBuf), Refusal> {
    let forbid = |code: &str, body: String| refuse(shown, "403 Forbidden", code, body);
    let roots = site.data_roots();
    let skipped = if roots.skipped.is_empty() {
        String::new()
    } else {
        format!(" (skipped: {})", roots.skipped.join("; "))
    };
    if roots.roots.is_empty() {
        return Err(forbid("R2c", format!("refused: {l} is a link, and links are followed only into a data root this machine declares in the data-roots file of its Ship of Tools config folder; none is declared{skipped}")));
    }
    let tracked = site.git.as_ref().is_some_and(|g| g.links(&site.url_root).contains(link));
    if !tracked {
        return Err(forbid("R2b", format!("refused: {l} is a link git does not track in this repo, so it is not followed (git add it, then open the page again)")));
    }
    let Ok(t) = site.url_root.join(link).canonicalize() else {
        return Err(refuse(
            shown,
            "404 Not Found",
            "R5",
            format!("the linked folder {l} does not resolve on this host (is its share mounted?)"),
        ));
    };
    let Some(d) = roots.roots.iter().find(|r| t.starts_with(r)) else {
        return Err(forbid("R3", format!("refused: {l} is a tracked link, but its target is not under a data root declared in this machine's data-roots file{skipped}")));
    };
    let Ok(f) = q.canonicalize() else {
        return Err(not_found());
    };
    if !f.starts_with(&t) {
        return Err(forbid("R4", format!("refused: {shown} leaves the linked folder {l} through a link inside it")));
    }
    let rel = f.strip_prefix(d).expect("f is under t, which is under d");
    let file = open_checked(d, rel, shown)?;
    Ok((file, f))
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
    let mut range_hdr: Option<String> = None;
    for line in lines {
        if let Some((name, val)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("cookie") {
                cookie_hdr = Some(val.trim().to_string());
            } else if name.trim().eq_ignore_ascii_case("range") {
                range_hdr = Some(val.trim().to_string());
            }
        }
    }

    // Strip any query string / fragment, then resolve per serving mode
    // (ADR 0029): Prefix splits off the leading `/<nonce>/` segment to pick
    // the connection's root; a Pool port serves its assigned root whole, so
    // root-relative links resolve on that origin. `set_cookie`, if `Some`, is
    // a `Set-Cookie` header value the eventual 200 response must include.
    let raw_path = target.split(|c| c == '?' || c == '#').next().unwrap_or("");
    let (site, rest, set_cookie) = match mode {
        ServeMode::Pool(port) => {
            let Some((site, secret)) = pool_entry_for(port) else {
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
                    &mut stream,
                    "404 Not Found",
                    "no site open for this link (it may have disconnected, or this \
                     site uses root-relative links and should open via a pool port)",
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
        Err(r) => return write_simple(&mut stream, r.status, &r.body).await,
    };

    // no-cache so a rebuilt site is picked up without a hard browser refresh.
    let mut extra = String::from("Cache-Control: no-cache\r\n");
    if let Some(cookie) = &set_cookie {
        extra.push_str(&format!("Set-Cookie: {cookie}\r\n"));
    }
    serve_file(
        &mut stream,
        method == "HEAD",
        tokio::fs::File::from_std(file),
        content_type(&resolved),
        range_hdr.as_deref(),
        &extra,
    )
    .await
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
        let (p1, s1) = assign_pool_port(S + 1, Site::plain(PathBuf::from("/a"))).unwrap();
        let (p2, s2) = assign_pool_port(S + 2, Site::plain(PathBuf::from("/b"))).unwrap();
        let (p3, _s3) = assign_pool_port(S + 3, Site::plain(PathBuf::from("/c"))).unwrap();
        let (p4, _s4) = assign_pool_port(S + 4, Site::plain(PathBuf::from("/d"))).unwrap();
        assert_eq!(
            vec![p1, p2, p3, p4],
            (base + 1..=base + POOL_SIZE).collect::<Vec<_>>()
        );
        // Distinct sites get distinct secrets.
        assert_ne!(s1, s2);
        // A re-open by an existing owner REPOINTS its port, not a new one,
        // and mints a FRESH secret — the old one (and any cookie it set)
        // stops working (security review).
        let (p2_again, s2_again) = assign_pool_port(S + 2, Site::plain(PathBuf::from("/b2"))).unwrap();
        assert_eq!(p2_again, p2);
        assert_ne!(s2_again, s2);
        assert_eq!(
            pool_entry_for(p2).map(|(r, _)| r.content_root.clone()),
            Some(PathBuf::from("/b2"))
        );
        // Fifth connection: exhausted.
        assert_eq!(assign_pool_port(S + 5, Site::plain(PathBuf::from("/e"))), None);
        assert_eq!(pool_in_use(), 4);
        // Disconnect frees exactly the owner's port; next claim gets it.
        remove_root(S + 3);
        assert_eq!(pool_in_use(), 3);
        let (p5, _s5) = assign_pool_port(S + 5, Site::plain(PathBuf::from("/e"))).unwrap();
        assert_eq!(p5, p3);
        reset();
    }
}

#[cfg(test)]
mod bind_fallback_tests {
    use super::*;

    /// Preferred port taken → `spawn` retries, then falls back to an
    /// OS-assigned port and records it for `bound_site_port()`.
    /// `start_paused` collapses the 10×300ms retry loop's sleeps.
    #[tokio::test(start_paused = true)]
    async fn spawn_falls_back_to_ephemeral_when_preferred_taken() {
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();
        spawn(taken).await.expect("fallback bind should succeed");
        let bound = bound_site_port().expect("actual port recorded");
        assert_ne!(bound, taken, "must not claim the squatted port");
    }
}

#[cfg(test)]
mod prefix_serve_tests {
    use super::*;

    /// Write `contents` at `root/rel`, creating parent directories as needed.
    fn write_asset(root: &Path, rel: &str, contents: &[u8]) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, contents).unwrap();
    }

    /// Issue a raw `GET path` against the prefix server at `addr` and return
    /// `(status_code, content_type, body)`. Minimal hand-rolled HTTP/1.1
    /// client — mirrors the server's own hand-rolled parsing, no crate needed
    /// for a handful of headers over loopback.
    ///
    /// `read_to_end` is bounded by a timeout (codex review): every response
    /// here closes the connection (`Connection: close`), so a healthy server
    /// always hits EOF quickly — a server-side regression that stops writing
    /// or stops closing should fail this test fast, not hang the CI job
    /// until its overall timeout.
    async fn get(addr: std::net::SocketAddr, path: &str) -> (u16, Option<String>, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_end(&mut raw),
        )
        .await
        .expect("server did not close the connection within 5s")
        .unwrap();
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("response must have a header/body separator");
        let head = String::from_utf8_lossy(&raw[..split]);
        let body = raw[split + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let ctype = lines.find_map(|l| {
            l.split_once(':').and_then(|(k, v)| {
                k.trim()
                    .eq_ignore_ascii_case("content-type")
                    .then(|| v.trim().to_string())
            })
        });
        (status, ctype, body)
    }

    /// Field-report regression: `docs.open`/`o` need the WHOLE site reachable
    /// under one root, not just the entry file — a synthetic site with a
    /// stylesheet, a script, an image, and a nested page all linked
    /// page-relatively, all served with the right content-type, plus the
    /// traversal guard refusing an escape past the root.
    #[tokio::test]
    async fn serves_index_and_relative_subresources_refuses_traversal() {
        // Unique by construction, not by clock: this name carried the clock
        // ALONE — not even a pid — so it was not separated across processes
        // either. Same premise as the race that reddened a macOS leg.
        let base = std::env::temp_dir()
            .join(format!("sot-site-serve-test-{}", sot_updater::unique::suffix()));
        let root = base.join("site");
        std::fs::create_dir_all(&root).unwrap();
        write_asset(
            &root,
            "index.html",
            b"<html><head><link rel=stylesheet href=assets/site.css>\
              <script src=assets/site.js></script></head>\
              <body><img src=figures/x.png><a href=pages/two.html>two</a></body></html>",
        );
        write_asset(&root, "assets/site.css", b"body{}");
        write_asset(&root, "assets/site.js", b"console.log(1)");
        write_asset(&root, "figures/x.png", &[0x89, b'P', b'N', b'G']);
        write_asset(&root, "pages/two.html", b"<html>two</html>");
        // Outside the root — a canonicalizing traversal target that EXISTS
        // (so the guard is proven by the root-containment check, not just
        // by the file happening not to exist).
        std::fs::write(base.join("secret.txt"), b"nope").unwrap();

        // Serial far outside anything other tests in this file produce
        // (pool_tests uses 9_000_000_00x) — parallel test runs share the
        // same static maps.
        const SERIAL: u64 = 8_000_000_001;
        let nonce = set_root(SERIAL, Site::plain(root.clone())).expect("set_root");

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(async move {
                            let _ = handle_conn(stream, ServeMode::Prefix).await;
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        let (status, ctype, body) = get(addr, &format!("/{nonce}/")).await;
        assert_eq!(status, 200, "index via directory-index");
        assert_eq!(ctype.as_deref(), Some("text/html; charset=utf-8"));
        assert!(!body.is_empty());

        for (rel, expected_ctype, expected_body) in [
            (
                "assets/site.css",
                "text/css; charset=utf-8",
                Some(&b"body{}"[..]),
            ),
            (
                "assets/site.js",
                "text/javascript; charset=utf-8",
                Some(&b"console.log(1)"[..]),
            ),
            ("figures/x.png", "image/png", None),
            (
                "pages/two.html",
                "text/html; charset=utf-8",
                Some(&b"<html>two</html>"[..]),
            ),
        ] {
            let (status, ctype, body) = get(addr, &format!("/{nonce}/{rel}")).await;
            assert_eq!(status, 200, "GET {rel} should 200");
            assert_eq!(
                ctype.as_deref(),
                Some(expected_ctype),
                "GET {rel} content-type"
            );
            if let Some(expected) = expected_body {
                assert_eq!(body, expected, "GET {rel} body");
            }
        }

        // A `../` escape past the root is refused even though the target
        // exists on disk.
        let (status, _, _) = get(addr, &format!("/{nonce}/../secret.txt")).await;
        assert_eq!(status, 403, "traversal escape must be refused");

        remove_root(SERIAL);
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(all(test, unix))]
mod linked_data_tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Far outside the serials other tests in this file use.
    static NEXT_SERIAL: AtomicU64 = AtomicU64::new(8_100_000_000);

    fn git(dir: &Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .status()
            .expect("run git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    const MOVIE_LEN: usize = 300_000;

    /// `repo/` (git) with `site/`, tracked links `data` and `bad` (plus a
    /// dangling tracked `dangling`), untracked links `loose` and `site/lnk`;
    /// `share/` holding the movie and an escaping link; `outside/`.
    struct Fixture {
        _dir: tempfile::TempDir,
        base: PathBuf,
        repo: PathBuf,
        share: PathBuf,
        outside: PathBuf,
        roots_file: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let repo = base.join("repo");
        let share = base.join("share");
        let outside = base.join("outside");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        write(&repo.join("site/index.html"), b"<html>idx</html>");
        write(
            &repo.join("site/exp/page.html"),
            b"<video src=\"../../data/results/r1/movie.mp4\">",
        );
        write(&repo.join("site/empty.js"), b"");
        write(&repo.join("other.txt"), b"repo file outside the site");
        let movie: Vec<u8> = (0..MOVIE_LEN).map(|i| (i % 251) as u8).collect();
        write(&share.join("results/r1/movie.mp4"), &movie);
        write(&outside.join("secret.txt"), b"secret");
        symlink("../share", repo.join("data")).unwrap();
        symlink("../outside", repo.join("bad")).unwrap();
        symlink("../nowhere", repo.join("dangling")).unwrap();
        symlink("../../../outside", share.join("results/r1/esc")).unwrap();
        symlink("../share", repo.join("loose")).unwrap();
        symlink("../../share", repo.join("site/lnk")).unwrap();
        git(&repo, &["add", "data", "bad", "dangling", "other.txt", "site"]);
        let roots_file = base.join("data-roots");
        std::fs::write(&roots_file, format!("{}\n", share.display())).unwrap();
        Fixture { _dir: dir, base, repo, share, outside, roots_file }
    }

    /// Serve `site` on loopback and return `(addr, nonce, serial)`.
    async fn serve(site: Site) -> (std::net::SocketAddr, String, u64) {
        let serial = NEXT_SERIAL.fetch_add(1, Ordering::SeqCst);
        let nonce = set_root(serial, site).expect("set_root");
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = handle_conn(stream, ServeMode::Prefix).await;
                });
            }
        });
        (addr, nonce, serial)
    }

    /// GET `path` with extra request headers; `(status, response head, body)`.
    async fn get(
        addr: std::net::SocketAddr,
        path: &str,
        headers: &[&str],
    ) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let extra: String = headers.iter().map(|h| format!("{h}\r\n")).collect();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\n{extra}Connection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
            .await
            .expect("server did not close the connection within 5s")
            .unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("separator");
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, head, raw[split + 4..].to_vec())
    }

    async fn widened(f: &Fixture) -> Site {
        Site::with_roots(f.repo.join("site"), f.repo.clone(), true, f.roots_file.clone()).await
    }

    fn body_text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    const MOVIE: &str = "data/results/r1/movie.mp4";

    #[tokio::test]
    async fn a_linked_movie_seeks_through_the_widened_url_space() {
        let f = fixture();
        let site = widened(&f).await;
        assert_eq!(site.url_path("exp/page.html"), "site/exp/page.html");
        assert_eq!(site.url_path(""), "site/");
        let (addr, nonce, serial) = serve(site).await;
        // The page itself, at its widened URL.
        let (st, _, body) = get(addr, &format!("/{nonce}/site/exp/page.html"), &[]).await;
        assert_eq!(st, 200);
        assert!(body_text(&body).contains("movie.mp4"));
        let (st, head, body) =
            get(addr, &format!("/{nonce}/{MOVIE}"), &["Range: bytes=1000-1999"]).await;
        assert_eq!(st, 206, "{head}");
        assert!(head.contains("Content-Range: bytes 1000-1999/300000"), "{head}");
        assert!(head.contains("Content-Type: video/mp4"), "{head}");
        assert!(head.contains("Accept-Ranges: bytes"), "{head}");
        let want: Vec<u8> = (1000..2000).map(|i| (i % 251) as u8).collect();
        assert_eq!(body, want);
        let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &[]).await;
        assert_eq!(st, 200);
        assert_eq!(body.len(), MOVIE_LEN);
        remove_root(serial);
    }

    #[tokio::test]
    async fn b_untracked_link_is_not_followed_r2b() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/loose/results/r1/movie.mp4"), &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("loose is a link git does not track"), "{}", body_text(&body));
        remove_root(serial);
    }

    #[tokio::test]
    async fn c_tracked_link_outside_a_data_root_is_refused_r3() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/bad/secret.txt"), &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("bad is a tracked link, but its target is not under a data root"));
        remove_root(serial);
    }

    #[tokio::test]
    async fn d_dotdot_is_refused_r1() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        for p in ["site/../../outside/secret.txt", "data/../outside/secret.txt"] {
            let (st, _, body) = get(addr, &format!("/{nonce}/{p}"), &[]).await;
            assert_eq!(st, 403, "{p}");
            assert!(body_text(&body).contains("\"..\" segment"), "{p}");
        }
        remove_root(serial);
    }

    #[tokio::test]
    async fn e_a_link_inside_the_linked_folder_is_not_followed_out_r4() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, body) =
            get(addr, &format!("/{nonce}/data/results/r1/esc/secret.txt"), &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("leaves the linked folder data through a link inside it"));
        remove_root(serial);
    }

    #[tokio::test]
    async fn f_git_dir_is_refused_on_the_widened_site_r0() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        for p in [".git/config", ".GIT/config", "site/.git/config"] {
            let (st, _, body) = get(addr, &format!("/{nonce}/{p}"), &[]).await;
            assert_eq!(st, 403, "{p}");
            assert!(body_text(&body).contains(".git is never served"), "{p}");
        }
        remove_root(serial);
    }

    /// Defect: when the site folder IS the repo top, the site server used to
    /// serve `.git/`. R0 closes it for a plain site too.
    #[tokio::test]
    async fn git_dir_of_a_repo_top_site_is_refused() {
        let f = fixture();
        assert!(f.repo.join(".git/config").is_file());
        write(&f.repo.join("index.html"), b"<html>top</html>");
        let (addr, nonce, serial) = serve(Site::plain(f.repo.clone())).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/index.html"), &[]).await;
        assert_eq!(st, 200, "the site itself still serves");
        assert!(body_text(&body).contains("top"));
        let (st, _, body) = get(addr, &format!("/{nonce}/.git/config"), &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains(".git is never served"));
        remove_root(serial);
    }

    #[tokio::test]
    async fn g_no_roots_means_todays_url_space_and_r2c() {
        let f = fixture();
        let empty = f.base.join("empty-roots");
        std::fs::write(&empty, "# nothing\n").unwrap();
        let site = Site::with_roots(f.repo.join("site"), f.repo.clone(), true, empty).await;
        assert_eq!(site.url_path("exp/page.html"), "exp/page.html");
        let (addr, nonce, serial) = serve(site).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/lnk/results/r1/movie.mp4"), &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("none is declared"), "{}", body_text(&body));
        // The site itself is untouched.
        let (st, _, _) = get(addr, &format!("/{nonce}/exp/page.html"), &[]).await;
        assert_eq!(st, 200);
        remove_root(serial);
    }

    #[tokio::test]
    async fn h_empty_file_has_content_length_zero_through_the_site_server() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, head, body) = get(addr, &format!("/{nonce}/site/empty.js"), &[]).await;
        assert_eq!(st, 200);
        assert!(head.contains("Content-Length: 0"), "{head}");
        assert!(body.is_empty());
        remove_root(serial);
    }

    #[test]
    fn i_read_data_roots_filters_and_canonicalizes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let home = base.join("h/user");
        let real = base.join("real");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        symlink(&real, base.join("alias")).unwrap();
        std::fs::write(base.join("afile"), b"x").unwrap();
        let file = base.join("data-roots");
        std::fs::write(
            &file,
            format!(
                "# comment\n\nrelative/dir\n{}\n/\n{}\n{}\n{}\n{}\n",
                base.join("missing").display(),
                home.display(),
                base.join("h").display(),
                base.join("afile").display(),
                base.join("alias").display(),
            ),
        )
        .unwrap();
        let got = read_data_roots(&file, Some(&home));
        assert_eq!(got.roots, vec![real.clone()], "only the real dir, canonical");
        assert_eq!(got.skipped.len(), 6, "{:?}", got.skipped);
        assert!(read_data_roots(&base.join("nope"), None).roots.is_empty());
    }

    #[tokio::test]
    async fn a_page_outside_the_site_is_r2a() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/other.txt"), &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("other.txt is outside the site folder"));
        remove_root(serial);
    }

    #[tokio::test]
    async fn a_dangling_tracked_link_is_r5() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/dangling/x.txt"), &[]).await;
        assert_eq!(st, 404);
        assert!(body_text(&body).contains("does not resolve on this host"));
        remove_root(serial);
    }

    #[test]
    fn a_symlink_swapped_in_at_open_time_is_r6() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        write(&base.join("real/f.txt"), b"x");
        write(&base.join("elsewhere/secret.txt"), b"s");
        symlink(base.join("elsewhere/secret.txt"), base.join("real/last")).unwrap();
        symlink(base.join("elsewhere"), base.join("real/mid")).unwrap();
        let r = open_checked(&base.join("real"), Path::new("last"), "last").err().expect("refused");
        assert_eq!(r.status, "403 Forbidden");
        assert!(r.body.contains("changed while it was being opened"));
        let r = open_checked(&base.join("real"), Path::new("mid/secret.txt"), "mid/secret.txt")
            .err()
            .expect("refused");
        assert!(r.body.contains("changed while it was being opened"));
        assert!(open_checked(&base.join("real"), Path::new("f.txt"), "f.txt").is_ok());
        // Missing is a plain 404, not R6.
        let r = open_checked(&base.join("real"), Path::new("nope"), "nope").err().unwrap();
        assert_eq!(r.status, "404 Not Found");
    }

    /// Freshness: the tracked-link set follows the repo index at every request.
    #[tokio::test]
    async fn untracking_a_link_between_requests_refuses_the_second_r2b() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, _) = get(addr, &format!("/{nonce}/{MOVIE}"), &["Range: bytes=0-9"]).await;
        assert_eq!(st, 206);
        git(&f.repo, &["rm", "--cached", "-q", "data"]);
        let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &["Range: bytes=0-9"]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("data is a link git does not track"));
        // And tracking it again is picked up too.
        git(&f.repo, &["add", "data"]);
        let (st, _, _) = get(addr, &format!("/{nonce}/{MOVIE}"), &["Range: bytes=0-9"]).await;
        assert_eq!(st, 206);
        remove_root(serial);
    }

    /// Freshness: the data-roots file is read at every request.
    #[tokio::test]
    async fn removing_a_root_between_requests_refuses_the_second_r3_then_r2c() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let url = format!("/{nonce}/{MOVIE}");
        let (st, _, _) = get(addr, &url, &["Range: bytes=0-9"]).await;
        assert_eq!(st, 206);
        // Another root remains: the target is no longer under any -> R3.
        std::fs::write(&f.roots_file, format!("{}\n", f.outside.display())).unwrap();
        let (st, _, body) = get(addr, &url, &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("target is not under a data root"));
        // Nothing left declared -> R2c.
        std::fs::write(&f.roots_file, "").unwrap();
        let (st, _, body) = get(addr, &url, &[]).await;
        assert_eq!(st, 403);
        assert!(body_text(&body).contains("none is declared"));
        remove_root(serial);
    }

    #[tokio::test]
    async fn a_skipped_root_is_named_in_the_refusal() {
        let f = fixture();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let ghost = f.base.join("ghost-share");
        std::fs::write(
            &f.roots_file,
            format!("{}\n{}\n", ghost.display(), f.outside.display()),
        )
        .unwrap();
        let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &[]).await;
        assert_eq!(st, 403);
        let t = body_text(&body);
        assert!(t.contains(&format!("skipped: {} (does not exist on this host)", ghost.display())), "{t}");
        // R2c names it too.
        std::fs::write(&f.roots_file, format!("{}\n", ghost.display())).unwrap();
        let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &[]).await;
        assert_eq!(st, 403);
        let t = body_text(&body);
        assert!(t.contains("none is declared") && t.contains("ghost-share"), "{t}");
        remove_root(serial);
    }

    /// B1: the repo top is the site; a tracked link reaches `.git`.
    #[tokio::test]
    async fn a_tracked_link_into_dot_git_is_refused_b1a() {
        let f = fixture();
        symlink("../.git", f.repo.join("site/g")).unwrap();
        git(&f.repo, &["add", "site/g"]);
        let (addr, nonce, serial) = serve(Site::plain(f.repo.clone())).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/site/g/config"), &[]).await;
        assert_eq!(st, 403, "{}", body_text(&body));
        assert!(body_text(&body).contains(".git is never served"));
        remove_root(serial);
    }

    /// B1: a mixed-case `.Git` directory reached through a link.
    #[tokio::test]
    async fn a_mixed_case_dot_git_behind_a_link_is_refused_b1b() {
        let f = fixture();
        write(&f.repo.join("meta/.Git/secret"), b"s");
        symlink("meta/.Git", f.repo.join("g2")).unwrap();
        git(&f.repo, &["add", "g2"]);
        let (addr, nonce, serial) = serve(Site::plain(f.repo.clone())).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/g2/secret"), &[]).await;
        assert_eq!(st, 403, "{}", body_text(&body));
        assert!(body_text(&body).contains(".git is never served"));
        remove_root(serial);
    }

    fn reads(site: &Site) -> usize {
        site.roots_reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// B2a: opening canonicalizes nothing, even for a root that is not there.
    #[tokio::test]
    async fn open_does_not_resolve_a_missing_root_b2a() {
        let f = fixture();
        let ghost = f.base.join("ghost-share");
        std::fs::write(&f.roots_file, format!("{}\n", ghost.display())).unwrap();
        let site =
            Site::with_roots(f.repo.join("site"), f.repo.clone(), true, f.roots_file.clone()).await;
        assert_eq!(reads(&site), 0);
    }

    /// B2b and B2c: one read for an unchanged file, a fresh one after a change.
    #[tokio::test]
    async fn roots_are_resolved_once_per_file_state_b2() {
        let f = fixture();
        let site = widened(&f).await;
        assert_eq!(reads(&site), 0);
        assert_eq!(site.data_roots().roots, vec![f.share.clone()]);
        assert_eq!(site.data_roots().roots, vec![f.share.clone()]);
        assert_eq!(reads(&site), 1);
        std::fs::write(&f.roots_file, format!("{}\n{}\n", f.share.display(), f.outside.display()))
            .unwrap();
        assert_eq!(site.data_roots().roots, vec![f.share.clone(), f.outside.clone()]);
        assert_eq!(reads(&site), 2);
    }

    #[tokio::test]
    async fn a_root_declared_through_a_symlinked_path_matches_its_canonical_target() {
        let f = fixture();
        let alias = f.base.join("share-alias");
        symlink(&f.share, &alias).unwrap();
        std::fs::write(&f.roots_file, format!("{}\n", alias.display())).unwrap();
        let (addr, nonce, serial) = serve(widened(&f).await).await;
        let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &[]).await;
        assert_eq!(st, 200);
        assert_eq!(body.len(), MOVIE_LEN);
        remove_root(serial);
    }
}
