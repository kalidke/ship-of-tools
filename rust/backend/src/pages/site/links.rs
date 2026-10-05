//! What a request path may open: the site's roots, the git link set, the data-roots file and the R0-R6 rules.

use super::*;

/// One open site: where ordinary files are served from, where the URL space
/// maps, and (unix, when this machine declares data roots and the folder is in
/// a git repo) the state needed to follow tracked links. See the header.
pub struct Site {
    /// Canonical S: the only place ordinary files are served from.
    pub(super) content_root: PathBuf,
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
    let (mut child, held) = crate::lifecycle::child_signal::process().spawn_std(&mut cmd).map_err(|e| (false, e.to_string()))?;
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
        match crate::lifecycle::contain::exited(&mut child, false) {
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
            .or_else(|| sot_log::host::state_dir::sot_config_dir().map(|d| d.join("data-roots")))
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
    pub(super) status: &'static str,
    pub(super) body: String,
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

#[cfg(all(test, unix))]
#[path = "links_tests.rs"]
mod tests;
