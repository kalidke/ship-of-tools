//! What a request path may open: the site's roots, the git link set, the folders a link may reach and the R-rules.

use super::*;

/// One open site: where ordinary files are served from, where the URL space
/// maps, and (unix, when the folder is in a git repo) the state needed to
/// follow tracked links. See the header.
pub struct Site {
    /// Canonical S: the only place ordinary files are served from.
    pub(super) content_root: PathBuf,
    /// Canonical root of the URL path space: S, or the repo top when widened.
    url_root: PathBuf,
    /// Where a tracked link may lead.
    #[cfg(unix)]
    bounds: Bounds,
    #[cfg(unix)]
    git: Option<GitState>,
}

/// The folders a tracked link may serve from, fixed by the operating systems,
/// not kept by a person: where Linux mounts shares and drives (`/mnt`,
/// `/media`) and where macOS mounts every volume but its own disk (`/Volumes`).
#[cfg(unix)]
const MOUNT_FOLDERS: [&str; 3] = ["/mnt", "/media", "/Volumes"];

/// Where a tracked link may lead: a file under a mount folder, outside the
/// home folder (which may itself lie under one), with no hidden part below the
/// mount folder (where a copy of a home keeps its keys). Both canonical.
#[cfg(unix)]
#[derive(Clone, Debug)]
pub(super) struct Bounds {
    mounts: Vec<PathBuf>,
    /// Each kept mount folder as written, for `as_written` (same order as `mounts`).
    written: Vec<PathBuf>,
    /// `None` when the daemon has no home folder (`home_folder`): then no link is followed.
    home: Option<PathBuf>,
}

#[cfg(unix)]
impl Bounds {
    /// Canonical forms of `mounts` that exist (a mount folder that resolves to
    /// `/` would make every file linked data) and of `home`.
    pub(super) fn new(mounts: &[&Path], home: Option<&Path>) -> Bounds {
        let kept: Vec<(PathBuf, PathBuf)> = mounts
            .iter()
            .filter_map(|m| Some((m.to_path_buf(), m.canonicalize().ok()?)))
            .filter(|(_, c)| c.parent().is_some())
            .collect();
        Bounds {
            written: kept.iter().map(|(w, _)| w.clone()).collect(),
            mounts: kept.into_iter().map(|(_, c)| c).collect(),
            home: home.and_then(|h| h.canonicalize().ok()),
        }
    }

    /// This process's bounds, resolved once. Only the mount folders themselves
    /// and the home folder are resolved, so a dead share stalls nothing here.
    fn of_process() -> Bounds {
        static BOUNDS: std::sync::OnceLock<Bounds> = std::sync::OnceLock::new();
        BOUNDS
            .get_or_init(|| Bounds::new(&MOUNT_FOLDERS.map(Path::new), crate::pages::home_folder().as_deref()))
            .clone()
    }

    /// The mount folder the canonical file `f` is opened beneath, or why it is
    /// not served (for the log: the page gets one answer either way).
    fn base_for(&self, f: &Path) -> Result<&Path, &'static str> {
        use std::os::unix::ffi::OsStrExt;
        let Some(home) = &self.home else { return Err("no home folder is known") };
        if f.starts_with(home) {
            return Err("in the home folder");
        }
        let Some(m) = self.mounts.iter().find(|m| f.starts_with(m)) else {
            return Err("outside the mount folders");
        };
        let rel = f.strip_prefix(m).expect("f starts with m");
        if rel.components().any(|c| c.as_os_str().as_bytes().starts_with(b".")) {
            return Err("hidden");
        }
        Ok(m)
    }

    /// The absolute `p` with its leading mount folder, as written (`/mnt`) or
    /// canonical, replaced by that folder's canonical path; `None` when it
    /// starts with no mount folder, so a target outside them is never looked up.
    fn as_written(&self, p: &Path) -> Option<PathBuf> {
        self.written
            .iter()
            .zip(&self.mounts)
            .find_map(|(w, c)| p.strip_prefix(w).or_else(|_| p.strip_prefix(c)).ok().map(|rest| c.join(rest)))
    }
}

/// The repo a site sits in, and the tracked-link set built from its index.
#[cfg(unix)]
struct GitState {
    top: PathBuf,
    index: PathBuf,
    /// False on a Mac without the Command Line Tools: git is never run, the
    /// repo top comes from a walk up for `.git`, and no link is followed.
    usable: bool,
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

/// Run `git` in `dir` with the repo-selecting env removed, stdin null, and a
/// 10 s limit. `Err(true)` = git ran and exited nonzero (not a repo, or a
/// refusal, first stderr line included); `Err(false)` = it could not run or
/// timed out.
#[cfg(unix)]
fn run_git(dir: &Path, args: &[&str]) -> Result<Vec<u8>, (bool, String)> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.fsmonitor=false"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    // Whatever git started dies with it, within the bound.
    let out = crate::lifecycle::child_signal::process().output(&mut cmd, Duration::from_secs(10)).map_err(|e| (false, e.to_string()))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let first = String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").to_string();
        Err((true, first))
    }
}

/// Why no link is followed where git cannot run.
const NO_GIT: &str = "linked data needs git; on macOS install the Command Line Tools, then restart the daemon";

/// Whether running `git` is safe. On macOS without the Command Line Tools,
/// `/usr/bin/git` is a stub that pops an install prompt at every run, so a repo
/// there tracks no links; everywhere else git is run as found.
#[cfg(unix)]
fn git_usable(macos: bool, command_line_tools: bool) -> bool {
    !macos || command_line_tools
}

#[cfg(unix)]
fn git_present() -> bool {
    static PRESENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *PRESENT.get_or_init(|| {
        let clt = cfg!(target_os = "macos") && {
            let mut cmd = std::process::Command::new("xcode-select");
            cmd.arg("-p").stdin(std::process::Stdio::null());
            crate::lifecycle::child_signal::process()
                .output(&mut cmd, Duration::from_secs(10))
                .is_ok_and(|o| o.status.success())
        };
        git_usable(cfg!(target_os = "macos"), clt)
    })
}

#[cfg(unix)]
impl GitState {
    /// The repo containing `dir`: its canonical top and index file. `None`
    /// (fail closed, no links) when it is not a repo or git will not answer.
    fn find(dir: &Path, git_ok: bool) -> Option<GitState> {
        if !git_ok {
            tracing::info!(dir = %dir.display(), "site_serve: no usable git (macOS Command Line Tools missing?), no links followed");
            let top = dir.ancestors().find(|a| a.join(".git").exists())?.to_path_buf();
            return Some(GitState { index: top.join(".git"), top, usable: false, cache: Mutex::new(LinkCache::default()) });
        }
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
        Some(GitState { top, index, usable: true, cache: Mutex::new(LinkCache::default()) })
    }

    /// Tracked symlinks under `url_root`, relative to it. Re-stats the index on
    /// every call and rebuilds the set only when the stamp changed, so a link
    /// untracked (or added) after the open is honoured at the next request.
    /// The stamp is taken BEFORE listing, so a write during the listing shows
    /// as changed next time.
    fn links(&self, url_root: &Path) -> BTreeSet<PathBuf> {
        if !self.usable {
            return BTreeSet::new();
        }
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
            #[cfg(unix)]
            bounds: Bounds::of_process(),
            #[cfg(unix)]
            git: None,
        }
    }

    /// Open a site for `docs.open`. `ws_root` bounds how far the URL space may
    /// widen; `widen` is false for root-relative sites (they keep S as their
    /// URL root).
    pub async fn open(content_root: PathBuf, ws_root: PathBuf, widen: bool) -> Site {
        let site = Site::plain(content_root);
        #[cfg(unix)]
        {
            // Widened in every repo, so a page's link above S reaches
            // `follow_link`, to be served or refused with words. No link
            // target is resolved here: a dead share must not stall open.
            let site = tokio::task::spawn_blocking(move || {
                let mut site = site;
                site.attach_git(&ws_root, widen, git_present());
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
    fn attach_git(&mut self, ws_root: &Path, widen: bool, git_ok: bool) {
        let Some(g) = GitState::find(&self.content_root, git_ok) else { return };
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
                if self.content_root.starts_with(&r) {
                    self.url_root = r;
                }
            }
        }
        self.git = Some(g);
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
/// the last `O_NOFOLLOW|O_NONBLOCK|O_NOCTTY` (a FIFO cannot hang the task, a
/// terminal never becomes its controlling one). A symlink
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
            flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY
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
/// first symlink below url_root must be a link git tracks (R2b, R2e where git
/// cannot run), the link's target and Q must each resolve inside `Bounds`, the
/// target naming a data folder (not `/` or a folder holding the home folder or
/// the repository) and Q lying outside the repository (R3: one answer for
/// everything else, a missing, private, hidden or unreadable file included),
/// and the open is `open_beneath` its mount folder (R6). Not unix: no link is
/// ever followed (R2d). R2a and R3 name the path as asked, so neither shows
/// whether a folder lies behind it.
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
    let asked = if parts.is_empty() { "index.html".to_string() } else { parts.join("/") };
    // A link whose target text leads out of the site folder and is not a plain
    // absolute path under a mount folder is answered before anything below it
    // is looked up.
    #[cfg(unix)]
    if let Some(link) = first_link(site, &parts) {
        if leads_out_by_text(site, &link) {
            let l = link.to_string_lossy().into_owned();
            tracked_gate(site, &link, &asked, &l)?;
            tracing::info!(path = asked, why = "the link's target is not an absolute path of plain names under a mount folder", "site_serve: a linked path is outside the bounds");
            return Err(refused_r3(&asked, &l));
        }
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

    // Slow path: the first symlink below url_root.
    let first = first_link(site, &parts);
    let found = first.is_some();
    let link = first.unwrap_or_default();
    if !found {
        // No existence oracle outside S: whether or not the path exists, is a
        // folder or has an index, it gets this one answer, named as asked.
        if q.starts_with(&site.content_root) {
            return Err(not_found());
        }
        return Err(refuse(
            &asked,
            "403 Forbidden",
            "R2a",
            format!("refused: {asked} is outside the site folder; only the site folder and the repo's tracked data links are served"),
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
    follow_link(site, &q, &asked, &shown, &link, &l)
}

/// R2e and R2b: git runs here, and tracks the link.
#[cfg(unix)]
fn tracked_gate(site: &Site, link: &Path, asked: &str, l: &str) -> Result<(), Refusal> {
    let forbid = |code: &str, body: String| refuse(asked, "403 Forbidden", code, body);
    if site.git.as_ref().is_some_and(|g| !g.usable) {
        return Err(forbid("R2e", format!("refused: {l} is a link, and {NO_GIT}")));
    }
    let tracked = site.git.as_ref().is_some_and(|g| g.links(&site.url_root).contains(link));
    if !tracked {
        return Err(forbid("R2b", format!("refused: {l} is a link git does not track in this repo, so it is not followed (git add it: it serves at the next request)")));
    }
    Ok(())
}

/// The first symlink on `parts` below the URL root, by `lstat` of each name.
fn first_link(site: &Site, parts: &[&str]) -> Option<PathBuf> {
    let mut cur = site.url_root.clone();
    let mut link = PathBuf::new();
    for p in parts {
        cur.push(p);
        link.push(p);
        match std::fs::symlink_metadata(&cur) {
            Err(_) => return None,
            Ok(m) if m.file_type().is_symlink() => return Some(link),
            Ok(_) => {}
        }
    }
    None
}

/// Whether `link`'s raw target text leads out of the content root and is not
/// the plain absolute path under a mount folder a link is judged by: such a
/// link serves nothing, so nothing below it is looked up. A target that stays
/// in the content root (by name) is left to the site's own fast path.
#[cfg(unix)]
fn leads_out_by_text(site: &Site, link: &Path) -> bool {
    use std::path::Component;
    let at = site.url_root.join(link);
    let Ok(raw) = std::fs::read_link(&at) else { return false };
    if plain_mount_path(&site.bounds, &raw).is_some() {
        return false;
    }
    let mut lexical = PathBuf::new();
    for c in at.parent().unwrap_or(Path::new("/")).join(&raw).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            c => lexical.push(c.as_os_str()),
        }
    }
    !lexical.starts_with(&site.content_root)
}

/// `raw` as the canonical path it names when it is absolute, made of plain
/// names, and starts with a mount folder (as written or canonical).
#[cfg(unix)]
fn plain_mount_path(bounds: &Bounds, raw: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let plain = raw.is_absolute() && raw.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
    plain.then(|| bounds.as_written(raw)).flatten()
}

#[cfg(unix)]
fn follow_link(
    site: &Site,
    q: &Path,
    asked: &str,
    shown: &str,
    link: &Path,
    l: &str,
) -> Result<(std::fs::File, PathBuf), Refusal> {
    tracked_gate(site, link, asked, l)?;
    // One answer whether the link's target or the file is missing, private or
    // hidden, so a committed link learns nothing about what exists there; the
    // log says which.
    let top = site.git.as_ref().map(|g| g.top.as_path());
    let (m, f) = match judge(site, q, link, top) {
        Ok(v) => v,
        Err(why) => {
            tracing::info!(path = asked, why, "site_serve: a linked path is outside the bounds");
            return Err(refused_r3(asked, l));
        }
    };
    let rel = f.strip_prefix(&m).expect("base_for found f under m");
    // A file past the bounds that will not open (unreadable, gone, a symlink on
    // its way) or is not a regular file (a FIFO, a socket) is the missing file's
    // answer.
    match open_checked(&m, rel, shown) {
        Ok(file) if file.metadata().map_or(false, |md| md.is_file()) && !on_kernel_filesystem(&file) => Ok((file, f)),
        _ => Err(refused_r3(asked, l)),
    }
}

/// A tracked link serves exactly the files under its written target, reached by
/// their written names, with no symlink anywhere below the link.
///
/// The link's target is judged as text (absolute, plain names only, starting
/// with a mount folder), then must equal its canonical path `t` (no symlink on
/// its way), lie in the bounds and outside the repository `top`. The file is
/// `t` joined with the names asked below the link; it must lie in the bounds
/// and outside the repository, whose files serve only from the site folder, and
/// is opened name by name without following a symlink, so any symlink below the
/// link answers as a missing file does. Returns the mount folder the file is
/// opened beneath, and the file; or why not.
#[cfg(unix)]
fn judge(site: &Site, q: &Path, link: &Path, top: Option<&Path>) -> Result<(PathBuf, PathBuf), &'static str> {
    let at = site.url_root.join(link);
    let raw = std::fs::read_link(&at).map_err(|_| "the link does not read")?;
    let Some(written) = plain_mount_path(&site.bounds, &raw) else {
        return Err("the link's target is not an absolute path of plain names under a mount folder");
    };
    let t = at.canonicalize().map_err(|_| "the link does not resolve")?;
    if written != t {
        return Err("the link's target runs through a symlink");
    }
    site.bounds.base_for(&t).map_err(|_| "the link points outside the bounds")?;
    if top.is_some_and(|r| t.starts_with(r)) {
        return Err("the link points into the repository");
    }
    let f = t.join(q.strip_prefix(&at).map_err(|_| "the request is not below the link")?);
    let m = site.bounds.base_for(&f)?.to_path_buf();
    if top.is_some_and(|r| f.starts_with(r)) {
        return Err("in the repository");
    }
    Ok((m, f))
}

/// Whether `f` lies on `/proc` or `/sys`, where a regular file can be the
/// daemon's environment (a root mounted under a mount folder with `/proc` bound
/// inside it). Device nodes of a devtmpfs are not regular files and are refused
/// by that check; its magic is tmpfs's, so it cannot be told by type. macOS has
/// no such filesystem with regular files.
#[cfg(unix)]
fn on_kernel_filesystem(f: &std::fs::File) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: fstatfs(2) on a live fd into a zeroed out-parameter.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatfs(f.as_raw_fd(), &mut st) };
        // Fail closed when the type cannot be read.
        return rc != 0 || [0x9fa0, 0x6265_6572].contains(&(st.f_type as i64));
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = f;
        false
    }
}

/// R3's one answer to every linked path that does not serve.
#[cfg(unix)]
fn refused_r3(asked: &str, l: &str) -> Refusal {
    let [a, b, c] = MOUNT_FOLDERS;
    refuse(asked, "403 Forbidden", "R3", format!("refused: {asked} is behind the tracked link {l}, and a link serves only a file that exists on this host under {a}, {b} or {c}, outside the home folder and hidden folders, through a link written as an absolute path into those folders, with no symlink below it (is its share mounted?)"))
}

#[cfg(all(test, unix))]
#[path = "links_tests.rs"]
mod tests;
