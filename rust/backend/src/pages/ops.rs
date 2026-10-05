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

/// `docs.open`'s site-root walk (v0.6.6): starting from `start` (a subpage's
/// own directory), climb toward the OUTERMOST ancestor — bounded by
/// `workspace_root`, never above it — that sits in an unbroken chain of
/// directories each directly holding an `index.html`/`index.htm`.
///
/// The chain's first (innermost) member need not be `start` itself: a
/// subpage's own directory (`<site>/api/` for `<site>/api/reference.html`)
/// usually has no index of its own — only the site root a few levels up
/// does — and that gap is normal, not a stop signal, so the search for the
/// first hit climbs through it freely. Once a hit is found, further climbing
/// requires an unbroken chain: an unrelated ancestor higher up that happens
/// to ALSO hold an index.html (a different, coincidental site one level
/// further out) is never folded in — the walk stops at the first ancestor
/// above the chain that lacks one.
///
/// Every directory visited is a `.parent()` of the already-canonical path the
/// caller confined, so no new symlink resolution happens on the way up; the
/// `workspace_root` bound (itself canonical) keeps the walk from ever
/// producing a root outside the workspace. Returns `None` if nothing from
/// `start` up to and including `workspace_root` has an index — the caller's
/// documented fallback.
async fn find_site_root(
    start: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Option<std::path::PathBuf> {
    async fn has_index(dir: &std::path::Path) -> bool {
        for name in ["index.html", "index.htm"] {
            if tokio::fs::metadata(dir.join(name))
                .await
                .map(|m| m.is_file())
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }

    // Find the first (innermost) ancestor — starting at `start` — that has
    // an index, tolerating any number of index-less directories below it.
    let mut cur = start.to_path_buf();
    let first_hit = loop {
        if has_index(&cur).await {
            break Some(cur.clone());
        }
        if cur == workspace_root {
            break None;
        }
        match cur.parent() {
            Some(p) if p.starts_with(workspace_root) => cur = p.to_path_buf(),
            _ => break None,
        }
    };
    let mut root = first_hit?;

    // Extend upward through the UNBROKEN chain above the first hit.
    loop {
        if root == workspace_root {
            break;
        }
        let Some(parent) = root.parent() else { break };
        if !parent.starts_with(workspace_root) || !has_index(parent).await {
            break;
        }
        root = parent.to_path_buf();
    }
    Some(root)
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
/// - cursor on a directory → serve it, open `/` (its `index.html`);
/// - cursor on `index.html`/`index.htm` → serve its parent, open `/`;
/// - cursor on any other file → walk UP from its directory to the OUTERMOST
///   ancestor (bounded by the workspace root) that sits in an unbroken chain
///   of directories each directly holding an `index.html`/`index.htm` (see
///   `find_site_root`) — that ancestor is the site root, and `rel` is the
///   path from it down to the file. A subpage like `<site>/api/reference.html`
///   is served at the SITE's root, not `api/`'s, so `../assets/x.css` from
///   inside it resolves instead of escaping the served root and 404ing (this
///   was the bug: rooting at the subpage's own directory). If no ancestor up
///   to the workspace root has an index (v0.6.6: e.g. a project-log page
///   whose assets sit in a sibling `assets/` one level up, with no per-page
///   index anywhere), this falls back to the PRE-FIX behaviour — root = the
///   file's own parent — as a documented limitation: a page in that shape
///   using a parent-relative asset link still 404s. Fixing that would mean
///   parsing the page's own HTML for its relative links, which this handler
///   deliberately does not do (no HTML heuristics for routing decisions).
///
/// URL root (v0.6.6): the rooting rule above picks the CONTENT root, where
/// ordinary files are served from. A page on the shared prefix server whose
/// repo tracks a symlink to data under a root this machine declares in its
/// `data-roots` file gets a URL space starting at the repo top instead, so
/// `../../data/x.mp4` reaches the link folder; the returned path is then
/// prefixed with the content root's place below it (`Site::url_path`). What is
/// served does not widen; see `pages/site/mod.rs`.
#[allow(clippy::too_many_lines, reason = "the docs.open handler: maps the path into the page server's URL space and replies; predates the 100-line limit")]
pub async fn handle_docs_open(
    req_id: u64,
    payload_json: serde_json::Value,
    session: &Session,
    serial: Option<u64>,
    workspaces: &Workspaces,
) -> Result<HandlerOutput> {
    let req: DocsOpenReq = serde_json::from_value(payload_json).context("docs.open payload")?;
    tracing::info!(path = %req.path, "docs.open");

    let err = |msg: String, code: &str| -> HandlerOutput {
        vec![(
            Frame::res(req_id, op::DOCS_OPEN, json!({ "error": msg, "code": code })),
            None,
        )]
    };

    // The requesting connection's serial keys its per-connection site root
    // internally (ADR 0029); `pages::site::set_root` mints the unguessable
    // nonce that actually becomes the URL's first path segment (security
    // review — see `pages/site/mod.rs`). `None` only if hello hasn't registered
    // the connection yet — it always precedes docs.open in practice.
    let serial = match serial {
        Some(s) => s,
        None => {
            return Ok(err(
                "no connection context for docs.open (hello not received yet)".into(),
                "no_conn",
            ))
        }
    };

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

    // Derive (site root, URL path, entry page) — all off the canonical path.
    // `canon_p.parent()` inherits confinement from the check above (a path
    // under a workspace root is still under it once its last component is
    // dropped), so no second canonicalize/check is needed for `root`.
    let (root, rel, entry): (std::path::PathBuf, String, std::path::PathBuf) = if meta.is_dir() {
        (canon_p.clone(), String::new(), canon_p.join("index.html"))
    } else {
        let parent = canon_p
            .parent()
            .unwrap_or_else(|| std::path::Path::new("/"))
            .to_path_buf();
        let fname = canon_p
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        if fname.eq_ignore_ascii_case("index.html") || fname.eq_ignore_ascii_case("index.htm") {
            (parent, String::new(), canon_p.clone())
        } else {
            match find_site_root(&parent, &ws_root).await {
                Some(site_root) => {
                    // `site_root` is an ancestor of `parent` (= `canon_p`'s
                    // own parent), found by walking UP from it, so `canon_p`
                    // always strips cleanly; the fallback is unreachable.
                    let rel = canon_p
                        .strip_prefix(&site_root)
                        .unwrap_or(canon_p.as_path())
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/");
                    (site_root, rel, canon_p.clone())
                }
                // No ancestor up to `ws_root` holds an index.html — documented
                // fallback (see the doc comment above): pre-fix behaviour.
                None => (parent, fname, canon_p.clone()),
            }
        }
    };

    // A directory must have an index to open at `/`.
    if meta.is_dir() {
        let has_index = tokio::fs::metadata(&entry)
            .await
            .map(|m| m.is_file())
            .unwrap_or(false);
        if !has_index {
            return Ok(err(
                format!("no index.html in {}", root.display()),
                "no_index",
            ));
        }
    }

    // Loud root-relative guard (ADR 0029). The per-connection scheme serves under
    // `/<serial>/`, so page-relative links resolve but ROOT-relative ones
    // (`/assets/x.css`) escape the prefix and 404. Documenter output is clean; a
    // a project's `__site` / genhtml coverage tree is not. Scan the entry HTML and
    // refuse with a clear error rather than serve a silently-broken page (Option B's
    // per-port pool is the deferred fix for those).
    let entry_is_html = entry
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
        .unwrap_or(false);
    // Root-relative detection is now a ROUTER, not a refusal (ADR 0029 Option B,
    // implemented per maintainer decision, 2026-07-03 — "make proper fix now"): a site whose
    // entry HTML uses `/asset`-style links can't ride the shared `/{serial}/`
    // prefix server (the link escapes the prefix and 404s), so it gets a
    // DEDICATED pool port — its own origin, served from its true root, both
    // link styles resolve. A project's `__site` / genhtml trees open via W again.
    // `root` is already canonical (the confinement check above), so this
    // reuses it directly instead of re-canonicalizing with an unchecked
    // fallback to a possibly-uncanonical path (security review — that
    // fallback was the TOCTOU: if a second canonicalize somehow failed, it
    // silently registered the raw, unverified root instead of erroring).
    let mut root_relative = false;
    if entry_is_html {
        if let Ok(bytes) = tokio::fs::read(&entry).await {
            // Cap the scan — an index is small, but a bundled SPA can be large.
            let head = &bytes[..bytes.len().min(512 * 1024)];
            root_relative = has_root_relative_refs(&String::from_utf8_lossy(head));
        }
    }
    // The site: content root `root`, and a URL space that reaches a tracked
    // data link's folder when the page rides the shared prefix server (a
    // root-relative site keeps its own origin, so it is never widened).
    let site = crate::pages::site::links::Site::open(root, ws_root, !root_relative).await;
    let rel = site.url_path(&rel);
    let mut pool_port: Option<(u16, String)> = None;
    let mut site = Some(site);
    if root_relative {
        match crate::pages::site::assign_pool_port(serial, site.take().expect("site")) {
            Some(assigned) => pool_port = Some(assigned),
            None => {
                return Ok(err(
                    format!(
                        "{} uses root-relative links and every dedicated \
                         port is busy ({} of {} in use by other \
                         connections), or a secure token couldn't be minted \
                         — close another root-relative site, reconnect, or retry",
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
        // Shared prefix server: point this connection's slot at the site root
        // and hand back a URL whose first path segment is the unguessable
        // nonce `set_root` minted (security review — not the raw serial).
        // `None` means the CSPRNG read failed — fail closed rather than mint
        // a guessable nonce.
        let Some(nonce) = crate::pages::site::set_root(serial, site.take().expect("site")) else {
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

/// Does this HTML contain ROOT-relative `href="/…"` / `src="/…"` references
/// (ADR 0029)? Protocol-relative `//host` URLs (external CDN) are NOT
/// root-relative and must not trip the guard. Both quote styles are checked.
fn has_root_relative_refs(html: &str) -> bool {
    for needle in ["href=\"/", "src=\"/", "href='/", "src='/"] {
        let mut from = 0;
        while let Some(pos) = html[from..].find(needle) {
            let after = from + pos + needle.len();
            // The char right after the leading `/`: another `/` means `//host`
            // (protocol-relative, external) — not a root-relative path.
            if html[after..].chars().next() != Some('/') {
                return true;
            }
            from = after;
        }
    }
    false
}

/// Run one `quarto render` to its end. `None` means `sig` fired first: the
/// signal has already killed the render's whole tree (quarto, and the engines
/// its executable chunks start), whether or not the launcher was reaped.
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
    sig: &'static crate::lifecycle::child_signal::Signal,
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
    cmd.env("QUARTO_JULIA", crate::sidecars::julia::resolve_bin().map_err(std::io::Error::other)?.0);
    let mut contained = sig.spawn(&mut cmd)?;
    let (mut out, mut errs) = (contained.stdout.take(), contained.stderr.take());
    let work = async {
        let (mut so, mut se) = (Vec::new(), Vec::new());
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
        status.map(|status| std::process::Output { status, stdout: so, stderr: se })
    };
    tokio::select! {
        done = work => done.map(Some),
        // The daemon is shutting down: the signal has already killed the
        // render's tree.
        _ = sig.fired() => Ok(None),
    }
}

/// `quarto.open` — render a Quarto/markdown doc to a self-contained HTML on
/// the backend host (which has quarto + the RAM) and return the bytes, base64.
/// `execute = false` (`o`) = `--no-execute`: fast, quarto-only, no code run.
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
    let out_name = format!("__sot-qmd-{req_id}.html");
    let html_path = parent.join(&out_name);

    let output = match run_quarto("quarto", parent, file_name, &out_name, req.execute, crate::lifecycle::child_signal::process()).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            let _ = tokio::fs::remove_file(&html_path).await;
            return err("the daemon is shutting down".to_string(), "shutting_down");
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail = stderr
            .lines()
            .rev()
            .take(10)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return err(
            format!(
                "quarto render failed (execute={}){}",
                req.execute,
                if tail.trim().is_empty() {
                    String::new()
                } else {
                    format!(":\n{tail}")
                }
            ),
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
mod find_site_root_tests {
    // `docs.open`'s site-root walk (v0.6.6), tested directly against real
    // temp directories rather than through the full handler: `find_site_root`
    // is a pure path-and-filesystem function with no `Session`/`Workspaces`/
    // `pages::site` dependency, so it's the cheap, isolated place to pin the
    // walk's boundary behaviour. `handle_docs_open` end to end has no
    // existing test harness in this crate (no test binds the real
    // `pages::site` listener `bound_site_port()` requires) — out of scope to
    // add here; see the report for what that leaves unverified.
    use super::find_site_root;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sot-find-site-root-test-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn climbs_through_index_less_dirs_to_a_distant_site_index() {
        // <ws>/site/index.html, subpage two levels down at
        // <ws>/site/guide/api/reference.html — `guide/` and `guide/api/`
        // have no index of their own, matching a typical Documenter/
        // project-log-style tree where only the top has one.
        let ws = scratch("climb");
        let site = ws.join("site");
        let sub = site.join("guide").join("api");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(site.join("index.html"), "<html></html>").unwrap();
        std::fs::write(sub.join("reference.html"), "<html></html>").unwrap();

        let root = find_site_root(&sub, &ws).await.expect("site root found");
        assert_eq!(root, site, "climbs past two index-less dirs to the site root");

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn stops_at_the_outermost_of_an_unbroken_chain() {
        // <ws>/site/index.html AND <ws>/index.html both exist (the outer one
        // stands in for an "unrelated parent that happens to hold one" —
        // e.g. a repo-level landing page one level above the real site).
        // The chain breaks between them (nothing between `site/` and `ws`
        // lacks an index here because they're adjacent, so pin a THIRD
        // level: <ws>/outer/site/index.html with <ws>/outer/ having none),
        // proving the walk stops at `site/`, not `outer/`'s parent.
        let ws = scratch("stop-outer");
        let outer = ws.join("outer");
        let site = outer.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<html></html>").unwrap();
        std::fs::write(ws.join("index.html"), "<html></html>").unwrap(); // unrelated
        std::fs::write(site.join("page.html"), "<html></html>").unwrap();

        let root = find_site_root(&site, &ws)
            .await
            .expect("site root found");
        assert_eq!(
            root, site,
            "must not walk past the outermost index.html (site/) into the \
             unrelated ws/index.html one level further out, across the gap \
             at outer/ which has no index of its own"
        );

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn no_index_anywhere_up_to_the_workspace_root_is_none() {
        // The project-log shape: a standalone page with a SIBLING assets/
        // dir, no index.html anywhere above it up to the workspace root.
        // This is the documented fallback case — `find_site_root` returns
        // `None` and the caller keeps the pre-fix parent-rooted behaviour.
        let ws = scratch("no-index");
        let page_dir = ws.join("journal");
        std::fs::create_dir_all(&page_dir).unwrap();
        std::fs::create_dir_all(ws.join("assets")).unwrap();
        std::fs::write(page_dir.join("2026-09-01.html"), "<html></html>").unwrap();

        assert_eq!(find_site_root(&page_dir, &ws).await, None);

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn site_root_exactly_at_the_workspace_root_stops_there() {
        // <ws>/index.html with the page directly inside <ws> — the walk
        // must find <ws> itself and never look above it.
        let ws = scratch("at-root");
        std::fs::write(ws.join("index.html"), "<html></html>").unwrap();
        std::fs::write(ws.join("page.html"), "<html></html>").unwrap();

        let root = find_site_root(&ws, &ws).await.expect("site root found");
        assert_eq!(root, ws, "the workspace root itself is the site root");

        let _ = std::fs::remove_dir_all(&ws);
    }
}

#[cfg(all(test, unix))]
mod quarto_shutdown_tests {
    use super::*;
    use std::time::Duration;

    /// Holds `ENV_TEST_LOCK` and pins the julia the daemon resolves, so a
    /// render does not depend on a julia being installed; restores both
    /// variables it touches.
    struct JuliaPin {
        _serial: std::sync::MutexGuard<'static, ()>,
        saved: [(&'static str, Option<std::ffi::OsString>); 2],
    }

    impl JuliaPin {
        fn new(julia: &std::path::Path, quarto_julia: Option<&str>) -> Self {
            let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let saved = [
                ("SOT_JULIA_BIN", std::env::var_os("SOT_JULIA_BIN")),
                ("QUARTO_JULIA", std::env::var_os("QUARTO_JULIA")),
            ];
            std::env::set_var("SOT_JULIA_BIN", julia);
            match quarto_julia {
                Some(v) => std::env::set_var("QUARTO_JULIA", v),
                None => std::env::remove_var("QUARTO_JULIA"),
            }
            Self { _serial, saved }
        }
    }

    impl Drop for JuliaPin {
        fn drop(&mut self) {
            for (key, value) in self.saved.iter_mut() {
                match value.take() {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    /// SIGKILLs the pid a stub wrote to `file`, on every path out of the test.
    struct KillSleeper(std::path::PathBuf);

    impl Drop for KillSleeper {
        fn drop(&mut self) {
            if let Ok(pid) = std::fs::read_to_string(&self.0) {
                if let Ok(pid) = pid.trim().parse::<i32>() {
                    // SAFETY: a plain signal to a sleeper this test started.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
            }
        }
    }

    /// Run `stub` (a script body) as `quarto render` to its end on a private signal.
    fn run_stub_quarto(dir: &std::path::Path, body: &str) -> std::io::Result<Option<std::process::Output>> {
        let stub = dir.join("stub-quarto");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\n{body}"));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let program = stub.to_string_lossy().into_owned();
        rt.block_on(run_quarto(&program, dir, std::ffi::OsStr::new("doc.qmd"), "out.html", true, sig))
    }

    /// The shutdown signal kills the render's whole process group, engines included.
    #[tokio::test]
    async fn shutdown_kills_the_quarto_render_and_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let _pin = JuliaPin::new(&dir.path().join("julia"), None);
        let pid_file = dir.path().join("engine.pid");
        let stub = dir.path().join("stub-quarto");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n", pid_file.display()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let task = tokio::spawn(async move {
            run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, sig).await
        });
        let began = std::time::Instant::now();
        while sig.live() == 0 || !pid_file.exists() || std::fs::read_to_string(&pid_file).unwrap().trim().is_empty() {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub render never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let engine: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        sig.fire();
        let done = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the render outlived the shutdown")
            .expect("render task")
            .expect("run_quarto");
        assert!(done.is_none(), "a killed render has no output");
        assert_eq!(sig.live(), 0);
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only probes the pid.
            unsafe { libc::kill(engine, 0) != 0 }
        });
        assert!(gone, "the engine child survived the shutdown");
    }

    /// An engine that holds the render's pipes after its launcher exits dies
    /// with the launcher's tree, so the render ends by itself and no signal
    /// is needed.
    #[tokio::test]
    async fn quarto_engine_dies_when_its_launcher_exits() {
        let dir = tempfile::tempdir().unwrap();
        let _pin = JuliaPin::new(&dir.path().join("julia"), None);
        let pid_file = dir.path().join("engine.pid");
        let _kill = KillSleeper(pid_file.clone());
        let stub = dir.path().join("stub-quarto");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nsleep 3102 &\necho $! > {}\nexit 0\n", pid_file.display()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let task = tokio::spawn(async move {
            run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, sig).await
        });
        let done = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the render outlived its launcher's exit")
            .expect("render task")
            .expect("run_quarto");
        assert!(done.is_some_and(|out| out.status.success()), "the render did not finish on its own");
        assert_eq!(sig.live(), 0);
        let engine: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
        let gone = (0..150).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            // SAFETY: signal 0 only probes the pid.
            unsafe { libc::kill(engine, 0) != 0 }
        });
        assert!(gone, "the engine survived its launcher's exit");
    }

    /// Quarto runs `QUARTO_JULIA` for its julia engine and otherwise a bare
    /// `julia`, which on Windows can be an app-execution alias that runs
    /// outside the daemon's containment; the daemon hands it the one julia
    /// it resolves for every other child.
    #[test]
    fn quarto_is_given_the_daemons_julia() {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved = (std::env::var_os("SOT_JULIA_BIN"), std::env::var_os("QUARTO_JULIA"));
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("qj");
        let stub = dir.path().join("stub-quarto");
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\nprintf '%s' \"$QUARTO_JULIA\" > {}\n", out.display()));
        let julia = dir.path().join("julia");
        std::env::set_var("SOT_JULIA_BIN", &julia);
        std::env::remove_var("QUARTO_JULIA");
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ran = rt.block_on(run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, sig));
        for (key, value) in [("SOT_JULIA_BIN", saved.0), ("QUARTO_JULIA", saved.1)] {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        ran.expect("run_quarto");
        assert_eq!(
            std::fs::read_to_string(&out).unwrap_or_default(),
            julia.to_string_lossy(),
            "quarto was not given the daemon's julia"
        );
    }

    /// A group's number is its leader's pid and is free the moment the
    /// leader is reaped: a tree still held after that names whatever process
    /// next takes the number. The launcher here exits at once; a descendant
    /// in its own session keeps the render's pipes open, so the render is
    /// still running when the registry is read.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn quarto_never_holds_a_freed_group_number() {
        let dir = tempfile::tempdir().unwrap();
        let _pin = JuliaPin::new(&dir.path().join("julia"), None);
        let sleeper = dir.path().join("sleeper");
        let _kill = KillSleeper(sleeper.clone());
        let stub = dir.path().join("stub-quarto");
        // The pid is written once the sleeper has its own session: until then a kill of the launcher's group takes it too.
        let body = "setsid sleep 3106 &\np=$!\nwhile [ \"$(cut -d' ' -f6 /proc/$p/stat)\" = \"$(cut -d' ' -f6 /proc/$$/stat)\" ]; do sleep 0.02; done\n";
        sot_log::test_exec::write_executable(&stub, format!("#!/bin/sh\n{body}echo $p > {}\nexit 0\n", sleeper.display()));
        let sig: &'static crate::lifecycle::child_signal::Signal = Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()));
        let (program, cwd) = (stub.to_string_lossy().into_owned(), dir.path().to_path_buf());
        let task = tokio::spawn(async move {
            run_quarto(&program, &cwd, std::ffi::OsStr::new("doc.qmd"), "out.html", true, sig).await
        });
        let began = std::time::Instant::now();
        while std::fs::read_to_string(&sleeper).map(|s| s.trim().is_empty()).unwrap_or(true) {
            assert!(began.elapsed() < Duration::from_secs(5), "the stub render never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!task.is_finished(), "the render ended while a descendant held its pipes");
        for group in sig.held_groups() {
            // SAFETY: signal 0 only probes the pid.
            assert_eq!(unsafe { libc::kill(group, 0) }, 0, "a held tree names a process-group number nothing holds");
        }
        sig.fire();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the render outlived the shutdown")
            .expect("render task")
            .expect("run_quarto");
    }

    /// A julia the resolver refuses fails the render; nothing is started.
    #[test]
    fn quarto_refuses_what_the_resolver_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let _pin = JuliaPin::new(std::path::Path::new(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\julia.exe"), None);
        let out = dir.path().join("qj");
        let ran = run_stub_quarto(dir.path(), &format!("printf '%s' \"$QUARTO_JULIA\" > {}\n", out.display()));
        assert!(!out.exists(), "a refused julia still rendered");
        let err = ran.expect_err("a refused julia must fail the render");
        assert!(err.to_string().contains("app-execution alias"), "unexpected error: {err}");
    }

    /// The daemon's julia replaces one the environment already names.
    #[test]
    fn an_inherited_quarto_julia_is_overridden() {
        let dir = tempfile::tempdir().unwrap();
        let julia = dir.path().join("julia");
        let _pin = JuliaPin::new(&julia, Some(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\julia.exe"));
        let out = dir.path().join("qj");
        run_stub_quarto(dir.path(), &format!("printf '%s' \"$QUARTO_JULIA\" > {}\n", out.display())).expect("run_quarto");
        assert_eq!(
            std::fs::read_to_string(&out).unwrap_or_default(),
            julia.to_string_lossy(),
            "quarto kept an inherited QUARTO_JULIA"
        );
    }
}
