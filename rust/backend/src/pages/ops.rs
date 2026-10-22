//! Page ops: video.open, docs.open (with its site-root walk) and quarto.open (render, then serve), each
//! answering with a loopback URL this daemon grants.

use anyhow::Context;
use anyhow::Result;
use serde_json::json;
use sot_protocol::op;
use sot_protocol::BlobDescriptor;
use sot_protocol::DocsOpenReq;
use sot_protocol::DocsOpenRes;
use sot_protocol::Frame;
use sot_protocol::QuartoOpenReq;
use sot_protocol::QuartoOpenRes;
use sot_protocol::VideoOpenReq;
use sot_protocol::VideoOpenRes;

use crate::files::confine::canonicalize_and_workspace_root;
use crate::server::reply::HandlerOutput;
use crate::session::Session;
use crate::rows::Workspaces;

/// `docs.open`'s site root for a page in `dir`: from `dir` up to the top of its
/// repository (the nearest folder holding `.git`) or `workspace_root`,
/// whichever comes first, the OUTERMOST folder that directly holds an
/// `index.html`, else `dir` itself. A folder with no index on the
/// way does not end the site: a project log's campaign page
/// (`<log>/experiments/<campaign>/run.html`, where `<campaign>/` holds an index
/// and `experiments/` none) is served from `<log>/`, so its `../../assets/`
/// resolve. The repository top does end it, so a page never takes in a folder
/// outside its repository. No site root contains `home`, since its pages could
/// read the ssh keys and the daemon's own token: `None` when `dir` does.
///
/// Every folder visited is a `.parent()` of the already-canonical path the
/// caller confined, and the walk never leaves `workspace_root`.
async fn find_site_root(
    dir: &std::path::Path,
    workspace_root: &std::path::Path,
    home: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    async fn has_index(dir: &std::path::Path) -> bool {
        tokio::fs::metadata(dir.join("index.html")).await.is_ok_and(|m| m.is_file())
    }

    let holds_home = |p: &std::path::Path| home.is_some_and(|h| h.starts_with(p));
    let mut root = None;
    let mut cur = dir.to_path_buf();
    while !holds_home(&cur) {
        if has_index(&cur).await {
            root = Some(cur.clone());
        }
        if cur == workspace_root || tokio::fs::symlink_metadata(cur.join(".git")).await.is_ok() {
            break;
        }
        match cur.parent() {
            Some(p) if p.starts_with(workspace_root) => cur = p.to_path_buf(),
            _ => break,
        }
    }
    root.or_else(|| (!holds_home(dir)).then(|| dir.to_path_buf()))
}

/// `video.open` — return a loopback HTTP URL for the cursored video file so
/// the frontend can hand it to the OS browser's HTML5 <video> (native
/// hardware decode + smooth playback, far better than streaming decoded frames
/// in-pane). The backend's `pages::video` server (spawned at startup) serves the
/// file with byte-range support; the launcher SSH-forwards the port. ADR 0018.
pub async fn handle_video_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: VideoOpenReq = serde_json::from_value(payload_json).context("video.open payload")?;
    tracing::info!(path = %req.path, "video.open");

    let path = std::path::Path::new(&req.path);
    if !crate::pages::video::is_servable_video(path) {
        let payload = json!({
            "error": format!("not a servable video file: {}", req.path),
            "code": "not_video",
        });
        return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
    }
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() => {}
        _ => {
            let payload = json!({
                "error": format!("no such file: {}", req.path),
                "code": "io_error",
            });
            return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
        }
    }

    // Register this ONE file under an opaque token rather than handing the
    // frontend a URL that embeds the raw filesystem path (security review:
    // the pages::video port has no auth of its own, so a URL shaped like
    // `http://127.0.0.1:1235/<abs-path>` let any local user GET any
    // owner-readable video — or, worse, anything else that path pointed at).
    // `None` means the CSPRNG read failed — fail closed rather than mint a
    // guessable token (security review).
    let Some(token) = crate::pages::video::register_video(path.to_path_buf()) else {
        let payload = json!({
            "error": "could not mint a secure grant token (system RNG unavailable) — try again",
            "code": "rng_unavailable",
        });
        return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
    };
    // ACTUAL bound port, never the preferred `video_port()`: when the
    // preferred bind lost to another user's daemon (shared host), a URL
    // built on the preferred port would send this user's grant token to the
    // OTHER user's video server — "no such grant" for the user, token leak
    // to a stranger's process (2026-07-23 shared-host incident).
    let Some(port) = crate::pages::video::bound_video_port() else {
        let payload = json!({
            "error": "video server is not running (both preferred and ephemeral binds failed at startup) — check the daemon log",
            "code": "video_server_down",
        });
        return Ok(vec![(Frame::res(req_id, op::VIDEO_OPEN, payload), None)]);
    };
    let url = format!("http://127.0.0.1:{port}/{token}");
    let res = VideoOpenRes { url };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::VIDEO_OPEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// `docs.open` — open a static site/page under the workspace root in the OS
/// browser with full CSS/JS/sub-page fidelity. (Op name is legacy from the
/// Documenter first cut; it now serves any directory, not just
/// `docs/build`.) `req.path` is the cursored file's absolute backend path;
/// the handler roots the `pages::site` server at that file's **site root**
/// (not just its own directory — see the rooting rule below) and returns the
/// URL. The launcher SSH-forwards the port. ADR 0024.
///
/// Confined to the workspace's project root (security review): `pages::site`'s
/// port has no auth of its own, so rooting it at an arbitrary absolute
/// directory would let `docs.open` turn it into a general-purpose file server
/// for anything the daemon's owner can read, reachable by any local user.
///
/// Rooting rule (so both relative AND root-relative `/asset` links resolve):
/// the cursored file, or the cursored folder's `index.html`, is served from the
/// site it belongs to: the outermost folder holding an index between it and
/// the top of its repository or the workspace root (`find_site_root`), opened
/// at its place below that folder (a file at its own name, a folder at its folder's URL). So
/// `<site>/api/reference.html`, `<log>/journal/index.html`, `<log>/journal/`
/// and `<log>/experiments/<campaign>/run.html` all resolve `../assets/x.css`
/// against their site. With no index on the way, the file's own folder is
/// served, and a page whose assets sit beside that folder is not given them. Measured on Unix, for a page on the
/// prefix server: in a repository their request is a 403 "outside the site folder", outside one a 404 "no site open for
/// this link" (the `../` climbs above the nonce). Not measured: Windows, where the URL root is never widened (that is
/// `cfg(unix)`), so by reading the code the answer is the 404 in a repository too; and a root-relative (pool) page,
/// where by reading the code `../x` normalizes under the content root and is a 404 "no such file". The
/// handler does not parse a page's HTML for its links. A page whose folder
/// holds the home folder is refused, never served.
///
/// URL root (v0.6.6): the rooting rule above picks the CONTENT root, where
/// ordinary files are served from. A page on the shared prefix server in a
/// git repo gets a URL space starting at the repo top instead, so
/// `../../data/x.mp4` reaches the link folder, which serves it only under a
/// mount folder, outside the home folder and hidden folders, and otherwise
/// refuses it with words; the returned path is then prefixed with the content root's
/// place below it (`Site::url_path`). What is served does not widen; see
/// `pages/site/mod.rs`.
#[allow(clippy::too_many_lines, reason = "the docs.open handler: maps the path into the page server's URL space and replies; predates the 100-line limit")]
pub async fn handle_docs_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    owner: u64,
    workspaces: &Workspaces,
    clients: &crate::clients::Clients,
) -> Result<HandlerOutput> {
    let req: DocsOpenReq = serde_json::from_value(payload_json).context("docs.open payload")?;
    tracing::info!(path = %req.path, "docs.open");

    let err = |msg: String, code: &str| -> HandlerOutput {
        vec![(
            Frame::res(req_id, op::DOCS_OPEN, json!({ "error": msg, "code": code })),
            None,
        )]
    };

    // `owner` keys the site internally (ADR 0029): the requesting connection's window's
    // (`ClientGuard::site_owner`), so a redial keeps it; `pages::site::set_root` mints the
    // unguessable nonce that actually becomes the URL's first path segment (security review —
    // see `pages/site/mod.rs`).
    if req.path.is_empty() {
        return Ok(err(
            "nothing selected to open — put the cursor on an .html file or a directory".into(),
            "no_selection",
        ));
    }

    let p = std::path::Path::new(&req.path);
    let meta = match tokio::fs::metadata(p).await {
        Ok(m) => m,
        Err(e) => return Ok(err(format!("no such path: {} ({e})", req.path), "io_error")),
    };

    // Confine the SELECTED PATH ITSELF to a KNOWN workspace (security
    // review), not just its derived parent/root: canonicalizing only the
    // parent let a symlink sitting AT `p` escape the workspace — the parent
    // dir canonicalizes fine even though the symlink's target doesn't — while
    // the raw `p` still got read/scanned as `entry` below (exfil via e.g. a
    // symlinked `page.html -> /home/victim/secret.html`, or a DoS via one
    // pointed at `/dev/zero` — `tokio::fs::read` never sees EOF on that).
    // Canonicalize `p` itself here, ONCE, under ANY currently-registered
    // workspace (not just the default — same check as `pluto.open`'s), and
    // derive (root, rel, entry) from THIS canonical path for everything
    // downstream. The raw `p`/`req.path` is never read or scanned again below.
    // Also keep that workspace's own canonical root (`ws_root`): the
    // site-root walk below (v0.6.6) needs a bound it cannot climb past, and
    // this is the SAME confinement check's own root, not a second lookup
    // that could disagree with it.
    let (canon_p, ws_root) = match canonicalize_and_workspace_root(p, workspaces) {
        Some(c) => c,
        None => {
            return Ok(err(
                format!("{} is outside every known workspace root", req.path),
                "outside_workspace",
            ));
        }
    };

    // Derive (site root, URL path, entry page), all off the canonical path; a
    // folder opens its index page. `dir` is `canon_p` or its parent, so it
    // inherits confinement from the check above, and `find_site_root` climbs
    // from it only inside `ws_root`.
    let (dir, page, entry) = if meta.is_dir() {
        (canon_p.clone(), String::new(), canon_p.join("index.html"))
    } else {
        let dir = canon_p
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/"))
            .to_path_buf();
        let fname = canon_p
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        (dir, fname, canon_p.clone())
    };
    let home = crate::pages::home_folder();
    let Some(root) = find_site_root(&dir, &ws_root, home.as_deref()).await else {
        return Ok(err(
            format!(
                "{} is not opened: its folder holds the home folder, whose keys a page there could read \
                 — move the page into a folder of its own",
                req.path
            ),
            "home_folder",
        ));
    };
    // A folder must hold an index page to open.
    if meta.is_dir() {
        let has_index = tokio::fs::metadata(&entry)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false);
        if !has_index {
            return Ok(err(
                format!("no index.html in {}", dir.display()),
                "no_index",
            ));
        }
    }

    // Root-relative detection is a ROUTER, not a refusal (ADR 0029 Option B,
    // the proper fix, decided 2026-07-03): a site whose
    // entry HTML uses `/asset`-style links can't ride the shared `/{nonce}/`
    // prefix server (the link escapes the prefix and 404s), so it gets a
    // DEDICATED pool port — its own origin, served from its true root, both
    // link styles resolve. A project's `__site` / genhtml trees open via W again.
    let links = root_relative_links(&entry).await;
    let root_relative = !links.is_empty();
    let root = narrow_to_links(root, &dir, &links).await;
    // `root` is `dir` or an ancestor of it, so the strip cannot fail.
    let below = dir
        .strip_prefix(&root)
        .unwrap_or(std::path::Path::new(""))
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let rel = if below.is_empty() { page } else { format!("{below}/{page}") };
    // The site: content root `root`, and a URL space that reaches a tracked
    // data link's folder when the page rides the shared prefix server (a
    // root-relative site keeps its own origin, so it is never widened).
    let site = crate::pages::site::links::Site::open(root, ws_root, !root_relative).await;
    let rel = site.url_path(&rel);
    let mut pool_port: Option<(u16, String)> = None;
    let mut site = Some(site);
    if root_relative {
        match crate::pages::site::assign_pool_port(owner, site.take().expect("site"), || clients.away_sites()) {
            Some(assigned) => pool_port = Some(assigned),
            None => {
                return Ok(err(
                    format!(
                        "{} uses root-relative links and every dedicated \
                         port is busy ({} of {} held by other open \
                         windows), or a secure token couldn't be minted \
                         — close another root-relative site, or retry",
                        entry.display(),
                        crate::pages::site::pool_in_use(),
                        crate::pages::site::POOL_SIZE,
                    ),
                    "root_relative_pool_busy",
                ));
            }
        }
    }

    let url = if let Some((port, secret)) = pool_port {
        // Dedicated origin: root-relative links resolve, but the port itself
        // has no auth (security review). The ONE-TIME secret in this URL
        // authenticates the FIRST request; the pool server then sets an
        // HttpOnly cookie so later same-page asset fetches — which can't
        // carry a query string — authenticate via the cookie instead. See
        // `pages/site/mod.rs`'s `ServeMode::Pool` auth check.
        format!(
            "http://127.0.0.1:{}/{}?secret={}",
            port,
            crate::pages::site::encode_url_path(&rel),
            secret,
        )
    } else {
        // Shared prefix server: point this owner's slot at the site root
        // and hand back a URL whose first path segment is the unguessable
        // nonce `set_root` minted (security review — not the raw serial).
        // `None` means the CSPRNG read failed — fail closed rather than mint
        // a guessable nonce.
        let Some(nonce) = crate::pages::site::set_root(owner, site.take().expect("site")) else {
            return Ok(err(
                "could not mint a secure site token (system RNG unavailable) — try again".into(),
                "rng_unavailable",
            ));
        };
        // ACTUAL bound port, never the preferred `site_port()` — same
        // reasoning as `video.open` above: on a shared host the preferred
        // port may belong to another user's daemon.
        let Some(port) = crate::pages::site::bound_site_port() else {
            return Ok(err(
                "static-site server is not running (both preferred and ephemeral binds failed at startup) — check the daemon log".into(),
                "site_server_down",
            ));
        };
        format!(
            "http://127.0.0.1:{}/{}/{}",
            port,
            nonce,
            crate::pages::site::encode_url_path(&rel),
        )
    };

    let res = DocsOpenRes { url };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::DOCS_OPEN, serde_json::to_value(res)?).with_rev(rev),
        None,
    )])
}

/// The root-relative `href="/…"` / `src="/…"` paths in the page `entry` (ADR 0029), each without its leading `/`,
/// query or fragment, in document order; none for a file that is not HTML or cannot be read. Protocol-relative `//host` URLs (external
/// CDN) are not root-relative. Both quote styles are checked.
async fn root_relative_links(entry: &std::path::Path) -> Vec<String> {
    let is_html = entry
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    if !is_html {
        return Vec::new();
    }
    let Ok(bytes) = tokio::fs::read(entry).await else {
        return Vec::new();
    };
    // Cap the scan — an index is small, but a bundled SPA can be large.
    root_relative_paths(&String::from_utf8_lossy(&bytes[..bytes.len().min(512 * 1024)]))
}

fn root_relative_paths(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (needle, quote) in [("href=\"/", '"'), ("src=\"/", '"'), ("href='/", '\''), ("src='/", '\'')] {
        let mut from = 0;
        while let Some(pos) = html[from..].find(needle) {
            let after = from + pos + needle.len();
            let rest = &html[after..];
            // Another `/` right after the leading one means `//host` (external), not a root-relative path.
            if !rest.starts_with('/') {
                let end = rest.find([quote, '?', '#']).unwrap_or(rest.len());
                found.push((after, rest[..end].to_string()));
            }
            from = after;
        }
    }
    found.sort();
    found.into_iter().map(|(_, path)| path).collect()
}

/// A link that can narrow a site: a relative path of plain names only. Anything else (`..`, `.`, an empty part, a drive
/// or root, a `\\` or `:`) is ignored, so no link names a path outside the folders the walk already holds.
fn is_plain_link(link: &str) -> bool {
    !link.is_empty()
        && !link.contains(['\\', ':'])
        && link.split('/').all(|c| !c.is_empty() && c != "." && c != "..")
        && std::path::Path::new(link).components().all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// The most distinct links `narrow_to_links` tries: each costs a canonicalize per folder on the walk, and a page's
/// scan holds up to 512 KB of links, tens of thousands of them. A build's own assets come well within the first few.
const NARROW_LINKS_TRIED: usize = 64;

/// A root-relative page's site root: the innermost folder from `dir` up to `root` that holds the file its first
/// plain link names (the distinct links in document order, the first `NARROW_LINKS_TRIED` of them, those naming no
/// file there skipped), else `root`: a build's `dist/`, not the source tree's index above it. A folder counts only
/// when the link's canonical path is the folder's path joined with the link, so there is no symlink anywhere on it:
/// a link through one never narrows, and the choice, which the page's URL shows, depends on no file outside `root`
/// and confirms no guess at the page's own absolute path. `dir` is canonical, so every ancestor of it is. It only
/// narrows `root`.
async fn narrow_to_links(root: std::path::PathBuf, dir: &std::path::Path, links: &[String]) -> std::path::PathBuf {
    let mut seen = std::collections::HashSet::new();
    let tried = links.iter().filter(|l| is_plain_link(l) && seen.insert(l.as_str())).take(NARROW_LINKS_TRIED);
    for link in tried {
        for folder in dir.ancestors().take_while(|a| a.starts_with(&root)) {
            let path = folder.join(link);
            let Ok(file) = tokio::fs::canonicalize(&path).await else {
                continue;
            };
            if file == path && tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_file()) {
                return folder.to_path_buf();
            }
        }
    }
    root
}

/// The longest a render may run, `--no-execute` included: the longest `repl.execute` run (`repl/execute.rs`). Quarto
/// 1.9's Julia engine starts its server even when no code runs, and on its first use installs and precompiles it, so a
/// shorter bound for the quick render would end a working first use.
const QUARTO_RUN_BOUND: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// The renders in progress, by document: one render of a document at a time, since quarto's intermediates are named
/// after the source and two renders of one document would corrupt each other.
static RENDERING: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// A document's render lock, held for one render. The table's entry goes with the last render that holds or waits on it.
struct DocumentLock {
    key: std::path::PathBuf,
    lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl DocumentLock {
    /// Waits for the holder's render, which ends within its own bound.
    async fn take(key: std::path::PathBuf) -> Self {
        let lock = RENDERING.lock().unwrap_or_else(|e| e.into_inner()).entry(key.clone()).or_default().clone();
        let guard = lock.clone().lock_owned().await;
        Self { key, lock, guard: Some(guard) }
    }
}

impl Drop for DocumentLock {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut table = RENDERING.lock().unwrap_or_else(|e| e.into_inner());
        // The table's own handle and this one: nobody else holds or waits.
        if std::sync::Arc::strong_count(&self.lock) <= 2 {
            table.remove(&self.key);
        }
    }
}

/// An output name no other render shares: the request id is per connection, so two connections can send one.
fn quarto_out_name() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    format!("__sot-qmd-{}-{}.html", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Run one `quarto render` to its end, within `within`, one render of a document at a time. `None` means `sig` fired
/// first: the signal has already killed the render's whole tree (quarto, and the engines its executable chunks
/// start), whether or not the launcher was reaped. A render past `within` has its tree killed and is a `TimedOut`
/// error naming the bound and the last lines it wrote to stderr.
/// Quarto is always given the daemon's own julia, replacing any
/// `QUARTO_JULIA` it inherited; a julia the resolver refuses fails the
/// render, `--no-execute` or not. The launcher is reaped only after its tree
/// is killed.
async fn run_quarto(
    program: &str,
    cwd: &std::path::Path,
    file_name: &std::ffi::OsStr,
    out_name: &str,
    execute: bool,
    within: std::time::Duration,
    sig: &'static sot_containment::Signal,
) -> std::io::Result<Option<std::process::Output>> {
    use tokio::io::AsyncReadExt;
    let mut cmd = tokio::process::Command::new(program);
    cmd.current_dir(cwd)
        .arg("render")
        .arg(file_name)
        .arg("--to")
        .arg("html")
        .arg("--embed-resources")
        .arg("--output")
        .arg(out_name);
    if !execute {
        cmd.arg("--no-execute");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    cmd.env("QUARTO_JULIA", crate::sidecars::julia::resolve_bin().map_err(|e| std::io::Error::other(e.to_string()))?.0);
    let _document = DocumentLock::take(cwd.join(file_name)).await;
    let mut contained = sig.spawn(&mut cmd)?;
    let (mut out, mut errs) = (contained.stdout.take(), contained.stderr.take());
    // Read into buffers that outlive the wait, so a render that runs past its bound still says what it wrote.
    let (mut so, mut se) = (Vec::new(), Vec::new());
    let work = async {
        let read_out = async {
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_end(&mut so).await;
            }
        };
        let read_err = async {
            if let Some(e) = errs.as_mut() {
                let _ = e.read_to_end(&mut se).await;
            }
        };
        let (_, _, status) = tokio::join!(read_out, read_err, contained.wait());
        status.map(|status| std::process::Output { status, stdout: std::mem::take(&mut so), stderr: std::mem::take(&mut se) })
    };
    tokio::select! {
        done = work => done.map(Some),
        // The daemon is shutting down: the signal has already killed the
        // render's tree.
        _ = sig.fired() => Ok(None),
        _ = tokio::time::sleep(within) => {
            let _ = contained.kill().await;
            let said = crate::sidecars::child::last_lines(
                String::from_utf8_lossy(&se).lines(),
                QUARTO_TIMEOUT_LINES,
                QUARTO_TIMEOUT_LINE_CHARS,
            );
            let bound = format!("quarto render did not finish within {within:?}");
            let message = if said.is_empty() { bound } else { format!("{bound}; its last stderr lines: {said}") };
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, message))
        }
    }
}

const QUARTO_TIMEOUT_LINES: usize = 5;
const QUARTO_TIMEOUT_LINE_CHARS: usize = 200;
const QUARTO_ERROR_LIMIT: usize = 65_536;
const QUARTO_TRANSPORT_HINT: &str = "Quarto read an incomplete Julia server transport line. Quarto 1.9.38 and later wait up to two seconds for the newline, so slow server startup can still cause this error. If using an older Quarto, upgrading to 1.9.38 or later may help.";

fn quarto_render_error(execute: bool, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let lines: Vec<_> = stderr.lines().collect();
    let tail_start = lines.len().saturating_sub(10);
    let first_error = lines.iter().position(|line| line.contains("ERROR"));
    let mut message = format!("quarto render failed (execute={execute})");
    let hint = if stderr.contains("Read invalid transport file that did not end with a newline") {
        QUARTO_TRANSPORT_HINT
    } else { "" };
    let selected: Vec<_> = lines.iter().enumerate()
        .filter(|(index, line)| *index >= tail_start || line.contains("ERROR"))
        .map(|(index, line)| (index, std::borrow::Cow::Borrowed(*line)))
        .collect();
    if selected.iter().all(|(_, line)| line.trim().is_empty()) {
        if !hint.is_empty() {
            message.push('\n');
            message.push_str(hint);
        }
        return message;
    }
    let hint_bytes = if hint.is_empty() { 0 } else { hint.len() + 1 };
    let selection_bytes = selected.iter().map(|(_, line)| line.len() + 1).sum::<usize>() - 1;
    let (retained, omitted) = if message.len() + 2 + selection_bytes + hint_bytes <= QUARTO_ERROR_LIMIT {
        (selected, 0)
    } else {
        quarto_bounded_lines(&lines, tail_start, first_error,
            QUARTO_ERROR_LIMIT - message.len() - 2 - hint_bytes)
    };
    message.push_str(":\n");
    let mut output = Vec::new();
    let mut marker = (omitted > 0).then(|| format!("[{omitted} more ERROR lines omitted]"));
    for (index, line) in retained {
        if index >= tail_start {
            if let Some(marker) = marker.take() { output.push(marker); }
        }
        output.push(line.into_owned());
    }
    message.push_str(&output.join("\n"));
    if !hint.is_empty() {
        message.push('\n');
        message.push_str(hint);
    }
    message
}

/// Reserve every mandatory occurrence before admitting a prefix of optional ERROR occurrences.
fn quarto_bounded_lines<'a>(
    lines: &[&'a str], tail_start: usize, first_error: Option<usize>, budget: usize,
) -> (Vec<(usize, std::borrow::Cow<'a, str>)>, usize) {
    let mandatory = |index: usize| index >= tail_start || Some(index) == first_error;
    let mut retained: Vec<_> = lines.iter().enumerate()
        .filter(|(index, _)| mandatory(*index))
        .map(|(index, line)| (index, std::borrow::Cow::Borrowed(*line))).collect();
    let optional: Vec<_> = lines.iter().enumerate()
        .filter(|(index, line)| !mandatory(*index) && line.contains("ERROR")).collect();
    let marker_bytes = if optional.is_empty() { 0 }
        else { format!("[{} more ERROR lines omitted]", optional.len()).len() + 1 };
    let line_budget = budget - (retained.len() - 1) - marker_bytes;
    let lengths: Vec<_> = retained.iter().map(|(_, line)| line.len()).collect();
    if lengths.iter().sum::<usize>() > line_budget {
        let allocations = quarto_line_allocations(&lengths, line_budget);
        for ((_, line), limit) in retained.iter_mut().zip(allocations) {
            *line = quarto_line_excerpt(line, limit);
        }
    }
    let used = retained.iter().map(|(_, line)| line.len()).sum::<usize>();
    let mut remaining = line_budget - used;
    let mut admitted = 0;
    for (index, line) in &optional {
        if line.len() + 1 > remaining { break; }
        remaining -= line.len() + 1;
        retained.push((*index, std::borrow::Cow::Borrowed(**line)));
        admitted += 1;
    }
    retained.sort_by_key(|(index, _)| *index);
    (retained, optional.len() - admitted)
}

/// Keep short mandatory lines complete, sharing the remaining budget among oversized lines.
fn quarto_line_allocations(lengths: &[usize], mut budget: usize) -> Vec<usize> {
    let mut allocations = vec![0; lengths.len()];
    let mut pending: Vec<_> = (0..lengths.len()).collect();
    while !pending.is_empty() {
        let share = budget / pending.len();
        if let Some(position) = pending.iter().position(|index| lengths[*index] <= share) {
            let index = pending.remove(position);
            allocations[index] = lengths[index];
            budget -= lengths[index];
        } else {
            let count = pending.len();
            for (position, index) in pending.into_iter().enumerate() {
                let allocation = budget / (count - position);
                allocations[index] = allocation;
                budget -= allocation;
            }
            break;
        }
    }
    allocations
}

fn quarto_line_excerpt<'a>(line: &std::borrow::Cow<'a, str>, budget: usize) -> std::borrow::Cow<'a, str> {
    if line.len() <= budget { return line.clone(); }
    const MARKER: &str = "[line truncated]";
    let available = budget - MARKER.len();
    let mut start = available / 2;
    let mut end = line.len() - (available - start);
    while !line.is_char_boundary(start) { start -= 1; }
    while !line.is_char_boundary(end) { end += 1; }
    std::borrow::Cow::Owned(format!("{}{MARKER}{}", &line[..start], &line[end..]))
}

/// `quarto.open` — render a Quarto/markdown doc to a self-contained HTML on
/// the backend host (which has quarto + the RAM) and return the bytes, base64.
/// `execute = false` (`o`) = `--no-execute`: no code chunk runs, though
/// Quarto 1.9 still starts its Julia engine (installing it on a first use).
/// `execute = true` (`O`) runs code chunks — needs the language kernels on
/// this host. Renders into a unique temp subdir of the doc's own directory so
/// relative resources resolve (and `--embed-resources` can inline them), then
/// deletes it — the user's tree is left untouched, never clobbering a
/// hand-rendered `<doc>.html`.
pub async fn handle_quarto_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
) -> Result<HandlerOutput> {
    let req: QuartoOpenReq = serde_json::from_value(payload_json).context("quarto.open payload")?;
    tracing::info!(path = %req.path, execute = req.execute, "quarto.open");

    let err = |msg: String, code: &str| -> Result<HandlerOutput> {
        Ok(vec![(
            Frame::res(
                req_id,
                op::QUARTO_OPEN,
                json!({ "error": msg, "code": code }),
            ),
            None,
        )])
    };

    let src = std::path::Path::new(&req.path);
    let (Some(parent), Some(file_name)) = (src.parent(), src.file_name()) else {
        return err(format!("bad path: {}", req.path), "bad_path");
    };
    match tokio::fs::metadata(src).await {
        Ok(m) if m.is_file() => {}
        _ => return err(format!("no such file: {}", req.path), "io_error"),
    }

    // Render to a unique temp output *file* in the doc's own dir (= cwd), then
    // delete it. Use `--output <name>`, NOT `--output-dir`: `--output-dir` puts
    // quarto into project-render mode, which creates a `.quarto` cache dir and
    // then exits 1 on a "directory not empty" cleanup race even though the HTML
    // rendered fine — and litters `.quarto` in the user's tree. `--output
    // <name>` renders single-file in place (exit 0, no `.quarto`), resolves
    // relative resources, and the distinctive temp name avoids clobbering a
    // user's hand-rendered `<doc>.html`.
    let out_name = quarto_out_name();
    let html_path = parent.join(&out_name);

    let output = match run_quarto("quarto", parent, file_name, &out_name, req.execute, QUARTO_RUN_BOUND, sot_containment::process()).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            let _ = tokio::fs::remove_file(&html_path).await;
            return err("the daemon is shutting down".to_string(), "shutting_down");
        }
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            tracing::warn!(error = %e, path = %req.path, "quarto.open timed out");
            let _ = tokio::fs::remove_file(&html_path).await;
            return err(e.to_string(), "quarto_render_failed");
        }
        Err(e) => {
            return err(
                format!("failed to spawn quarto (is it installed on this host?): {e}"),
                "spawn_failed",
            );
        }
    };
    if !output.status.success() {
        let _ = tokio::fs::remove_file(&html_path).await;
        return err(
            quarto_render_error(req.execute, &output.stderr),
            "quarto_render_failed",
        );
    }

    let bytes = match tokio::fs::read(&html_path).await {
        Ok(b) => b,
        Err(e) => {
            let _ = tokio::fs::remove_file(&html_path).await;
            return err(
                format!(
                    "quarto succeeded but output unreadable ({}): {e}",
                    html_path.display()
                ),
                "output_missing",
            );
        }
    };
    let _ = tokio::fs::remove_file(&html_path).await;

    // The HTML rides as the trailing blob, NOT base64 in the envelope: an
    // `--embed-resources` render routinely blows past the codec's 1 MiB
    // envelope cap (a 1.2 MiB doc base64'd to 1.6 MiB), which used to fail the
    // write and take the whole connection down mid-session. `len` MUST equal
    // the bytes handed to `write_frame` or the next frame desyncs onto raw
    // HTML — see codec's `file_chunk_blob_is_consumed_no_desync`. Mirrors
    // math.render. Dropping base64 also sheds its +33% inflation.
    let res = QuartoOpenRes {
        blob: BlobDescriptor {
            len: bytes.len() as u64,
            mime: "text/html".to_string(),
        },
    };
    let (_, rev) = session.snapshot().await;
    Ok(vec![(
        Frame::res(req_id, op::QUARTO_OPEN, serde_json::to_value(res)?).with_rev(rev),
        Some(bytes),
    )])
}

#[cfg(test)]
#[path = "ops_find_site_root_tests.rs"]
mod find_site_root_tests;

#[cfg(test)]
#[path = "ops_error_tests.rs"]
mod error_tests;

#[cfg(all(test, unix))]
#[path = "ops_quarto_shutdown_tests.rs"]
mod quarto_shutdown_tests;

/// A resolver error reaches the caller of `run_quarto` before any Quarto program is started, on every platform.
#[cfg(test)]
#[path = "ops_resolver_consumer_tests.rs"]
mod resolver_consumer_tests;

/// A render ends at its bound, one render of a document runs at a time, and each render has its own output name.
#[cfg(all(test, unix))]
#[path = "ops_quarto_bound_tests.rs"]
mod quarto_bound_tests;
