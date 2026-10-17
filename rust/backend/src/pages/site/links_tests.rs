//! Tests of the follow rule: the bounds of a tracked link, the R-rules and the tracked-link cache.

use super::*;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU64, Ordering};

const BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Far outside the serials other tests in this file use.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(8_100_000_000);

fn git(dir: &Path, args: &[&str]) {
    sot_log::test_wait::run_within(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE"),
        BOUND,
    )
    .ok();
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

const MOVIE_LEN: usize = 300_000;

/// `repo/` (git) with `site/`, tracked links `data` (to the share), `bad` (out
/// of the mount folder), `dangling` and `site/lnk` (to the share, from inside
/// the site folder), and the untracked link `loose`. `mnt/` stands for a mount
/// folder and holds `share/` (the movie, a link out of `mnt/` and one to
/// `mnt/other/`) and the home folder `home/u`; `outside/` lies beside `mnt/`.
struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    share: PathBuf,
    bounds: Bounds,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let repo = base.join("repo");
    let share = base.join("mnt/share");
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
    write(&base.join("mnt/other/fig.png"), b"png");
    write(&base.join("mnt/home/u/notes.txt"), b"home");
    write(&outside.join("secret.txt"), b"secret");
    symlink("../mnt/share", repo.join("data")).unwrap();
    symlink("../outside", repo.join("bad")).unwrap();
    symlink("../mnt/nowhere", repo.join("dangling")).unwrap();
    symlink("../../../../outside", share.join("results/r1/esc")).unwrap();
    symlink("../../../other", share.join("results/r1/side")).unwrap();
    symlink("../mnt/share", repo.join("loose")).unwrap();
    symlink("../../mnt/share", repo.join("site/lnk")).unwrap();
    git(&repo, &["add", "data", "bad", "dangling", "other.txt", "site"]);
    let bounds = Bounds::new(&[&base.join("mnt")], Some(base.join("mnt/home/u").as_path()));
    Fixture { _dir: dir, base, repo, share, bounds }
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

/// `Site::open` with the fixture's bounds in place of this process's.
async fn opened(f: &Fixture, site: PathBuf, ws: PathBuf) -> Site {
    let mut s = Site::open(site, ws, true).await;
    s.bounds = f.bounds.clone();
    s
}

async fn widened(f: &Fixture) -> Site {
    opened(f, f.repo.join("site"), f.repo.clone()).await
}

/// The one answer to a linked path a link may not serve, whether or not it exists.
fn r3(asked: &str, link: &str) -> String {
    format!("refused: {asked} is behind the tracked link {link}, and a link serves only a file that exists on this host under /mnt, /media or /Volumes, outside the home folder and hidden folders (is its share mounted?)")
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
async fn c_a_tracked_link_out_of_the_mount_folders_is_refused_r3() {
    let f = fixture();
    let (addr, nonce, serial) = serve(widened(&f).await).await;
    let (st, _, body) = get(addr, &format!("/{nonce}/bad/secret.txt"), &[]).await;
    assert_eq!(st, 403);
    assert_eq!(body_text(&body), r3("bad/secret.txt", "bad"));
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

/// A link inside the linked folder is followed within the bounds and refused out of them.
#[tokio::test]
async fn e_a_link_inside_the_linked_folder_serves_only_within_the_bounds() {
    let f = fixture();
    let (addr, nonce, serial) = serve(widened(&f).await).await;
    let (st, _, body) = get(addr, &format!("/{nonce}/data/results/r1/side/fig.png"), &[]).await;
    assert_eq!((st, body_text(&body)), (200, "png".to_string()));
    let asked = "data/results/r1/esc/secret.txt";
    let (st, _, body) = get(addr, &format!("/{nonce}/{asked}"), &[]).await;
    assert_eq!((st, body_text(&body)), (403, r3(asked, "data")));
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

/// The path a browser requests for `link` on the page at URL path `page`: `..` stops at `/`.
fn browser_path(page: &str, link: &str) -> String {
    let mut parts: Vec<&str> = page.split('/').filter(|p| !p.is_empty()).collect();
    parts.pop();
    for seg in link.split('/') {
        match seg {
            "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

/// The URL space widens in every repo, so the page's own link above its site folder reaches the server (a browser
/// would otherwise collapse it above the nonce, where the answer is that no site is open) and serves, with no list of
/// data folders anywhere; so does a tracked link inside the site folder.
#[tokio::test]
async fn g_a_page_link_serves_with_no_list() {
    let f = fixture();
    let site = widened(&f).await;
    let page = site.url_path("exp/page.html");
    let (addr, nonce, serial) = serve(site).await;
    let page = format!("/{nonce}/{page}");
    let (st, _, _) = get(addr, &page, &[]).await;
    assert_eq!(st, 200);
    let movie = browser_path(&page, "../../data/results/r1/movie.mp4");
    let (st, head, _) = get(addr, &movie, &["Range: bytes=0-9"]).await;
    assert_eq!(st, 206, "{head}");
    let (st, head, _) = get(addr, &format!("/{nonce}/site/lnk/results/r1/movie.mp4"), &["Range: bytes=0-9"]).await;
    assert_eq!(st, 206, "{head}");
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

/// The bounds of this process: none of the private places of an account is under a mount folder, and under one only a
/// path with a hidden part or in the home folder is refused.
#[test]
fn i_the_bounds_of_this_process_refuse_every_private_place() {
    let b = Bounds::of_process();
    let home = b.home.clone().expect("the home folder resolves");
    let mut private: Vec<PathBuf> = [
        "/proc/1/environ", "/proc/self", "/sys/kernel", "/etc/passwd", "/tmp/x", "/run/user/1/x", "/var/log/x",
        "/root/x", "/private/etc/x", "/Users/x/y", "/",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    private.extend([
        home.join(".ssh/id_ed25519"),
        home.join(".sot-comm/x"),
        home.join("repo/notes.txt"),
        home.parent().unwrap().join("other-user/x"),
    ]);
    private.extend(sot_log::host::state_dir::sot_state_dir());
    private.extend(sot_log::host::state_dir::sot_config_dir().map(|d| d.join("token")));
    private.push(sot_log::host::state_dir::runtime_sot_dir());
    for p in &private {
        assert!(b.base_for(p).is_err(), "{} is served", p.display());
    }
    for m in &b.mounts {
        assert_eq!(b.base_for(&m.join("share/fig.png")), Ok(m.as_path()));
        assert_eq!(b.base_for(&m.join("share/.ssh/id")), Err("hidden"));
    }
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(base.join("m/home/u")).unwrap();
    symlink(base.join("m"), base.join("alias")).unwrap();
    let home = base.join("m/home/u");
    let b = Bounds::new(&[&base.join("alias"), &base.join("missing"), Path::new("/")], Some(home.as_path()));
    assert_eq!(b.mounts, vec![base.join("m")], "canonical, existing, never /");
    assert_eq!(b.base_for(&base.join("m/home/u/notes.txt")), Err("in the home folder"));
    assert_eq!(b.base_for(&base.join("m/share/a.png")), Ok(base.join("m").as_path()));
    assert_eq!(Bounds::new(&[&base.join("m")], None).base_for(&base.join("m/a.png")), Err("no home folder is known"));
    // A link names a data folder: never `/`, nor a folder holding the home folder or the repository.
    let top = base.join("m/repos/r");
    for (t, wide) in [
        (PathBuf::from("/"), true),
        (home.clone(), true),
        (base.join("m/home"), true),
        (base.join("m"), true),
        (top.clone(), true),
        (base.join("m/repos"), true),
        (base.join("m/share"), false),
        (top.join("data"), false),
    ] {
        assert_eq!(b.too_wide(&t, Some(&top)), wide, "{}", t.display());
    }
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

/// Outside the site folder a path that reaches no link gets one answer, whether it exists, is a file or a folder, with
/// or without an `index.html`: a page's script learns nothing about the repo's files
/// beyond its own site and the names of links it asks for. A link's own name is the one thing the answer still shows.
#[tokio::test]
async fn outside_the_site_a_path_with_no_link_has_one_answer_b1() {
    let f = fixture();
    write(&f.repo.join(".ssh/id_ed25519"), b"key");
    write(&f.repo.join("plaindir/a.txt"), b"a");
    write(&f.repo.join("sitedir/index.html"), b"<html></html>");
    write(&f.repo.join("untracked.env"), b"x=1");
    let cases = [
        "other.txt",
        "untracked.env",
        "missing.env",
        ".ssh/id_ed25519",
        ".ssh/missing",
        "plaindir",
        "plaindir/a.txt",
        "sitedir",
        "nolink/x",
        "nolink",
    ];
    let (addr, nonce, serial) = serve(widened(&f).await).await;
    for c in cases {
        let (st, _, body) = get(addr, &format!("/{nonce}/{c}"), &[]).await;
        let body = body_text(&body);
        assert_eq!(st, 403, "{c}: {body}");
        assert_eq!(
            body,
            format!("refused: {c} is outside the site folder; only the site folder and the repo's tracked data links are served"),
            "{c}"
        );
    }
    // A link is named by the requester, so its refusal may differ; it is still a refusal.
    let (st, _, _) = get(addr, &format!("/{nonce}/loose/x"), &[]).await;
    assert_eq!(st, 403);
    remove_root(serial);
}

/// A repo whose only link was never committed still widens: the link is refused as untracked, and once it is
/// committed the next request serves it, with no reopen.
#[tokio::test]
async fn a_never_committed_link_is_r2b_then_served_once_added_s1() {
    let f = fixture();
    let repo = f.base.join("repo2");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    write(&repo.join("site/index.html"), b"<html></html>");
    git(&repo, &["add", "site"]);
    symlink("../mnt/share", repo.join("nas")).unwrap();
    let site = opened(&f, repo.join("site"), repo.clone()).await;
    let (addr, nonce, serial) = serve(site).await;
    let path = format!("/{nonce}/nas/results/r1/movie.mp4");
    let (st, _, body) = get(addr, &path, &[]).await;
    assert_eq!(st, 403, "{}", body_text(&body));
    assert!(body_text(&body).contains("nas is a link git does not track"), "{}", body_text(&body));
    assert!(body_text(&body).contains("git add it: it serves at the next request"), "{}", body_text(&body));
    git(&repo, &["add", "nas"]);
    let (st, _, body) = get(addr, &path, &[]).await;
    assert_eq!(st, 200, "{}", body_text(&body));
    assert_eq!(body.len(), MOVIE_LEN);
    remove_root(serial);
}

/// A tracked link into each private place, and to nothing, gets the one answer, whether or not its target exists: under
/// the fixture's bounds (its home folder lies under its mount folder) and under this process's own.
#[tokio::test]
async fn every_private_or_missing_target_has_one_answer() {
    let f = fixture();
    write(&f.share.join(".backup/key"), b"k");
    write(&f.share.join("results/.hidden.png"), b"h");
    let real_home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let mut real: Vec<PathBuf> = [
        "/proc/self/environ", "/proc/1/cmdline", "/etc/passwd", "/etc/hostname", "/sys/kernel", "/tmp", "/run",
        "/var/log", "/root",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    real.extend([real_home.join(".ssh/id_ed25519"), real_home.join(".sot-comm"), real_home.parent().unwrap().to_path_buf()]);
    real.extend(sot_log::host::state_dir::sot_state_dir());
    real.extend(sot_log::host::state_dir::sot_config_dir());
    real.push(sot_log::host::state_dir::runtime_sot_dir());
    let mut links: Vec<(String, PathBuf)> = Vec::new();
    for (i, t) in real.iter().enumerate() {
        links.push((format!("p{i}"), t.clone()));
        links.push((format!("p{i}x"), t.join("missing-x")));
    }
    let fake = [
        ("h1", "mnt/home/u/notes.txt"),
        ("h2", "mnt/home/u/missing.txt"),
        ("d1", "mnt/share/.backup/key"),
        ("d2", "mnt/share/.backup/missing"),
        ("d3", "mnt/share/results/.hidden.png"),
        ("o1", "outside/secret.txt"),
        ("o2", "outside/missing.txt"),
        ("n1", "mnt/nowhere/x"),
    ];
    for (name, rel) in fake {
        links.push((name.to_string(), f.base.join(rel)));
    }
    for (name, target) in &links {
        symlink(target, f.repo.join(name)).unwrap();
        git(&f.repo, &["add", name]);
    }
    for own_bounds in [false, true] {
        let mut site = widened(&f).await;
        if own_bounds {
            site.bounds = Bounds::of_process();
        }
        let (addr, nonce, serial) = serve(site).await;
        for (name, _) in links.iter().filter(|(n, _)| !own_bounds || n.starts_with('p')) {
            for asked in [name.clone(), format!("{name}/x")] {
                let (st, _, body) = get(addr, &format!("/{nonce}/{asked}"), &[]).await;
                assert_eq!((st, body_text(&body)), (403, r3(&asked, name)), "{asked}");
            }
        }
        // Behind `data`: a hidden folder and a missing file under the share.
        for asked in ["data/.backup/key", "data/.backup", "data/results/missing.mp4"] {
            let (st, _, body) = get(addr, &format!("/{nonce}/{asked}"), &[]).await;
            assert_eq!((st, body_text(&body)), (403, r3(asked, "data")), "{asked}");
        }
        remove_root(serial);
    }
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

/// A tracked link to `/`, to a folder holding the home folder or to one holding the repository is refused even for a
/// file the bounds allow; a file they refuse through it still gets R3's one answer.
#[tokio::test]
async fn a_link_to_root_home_or_above_the_repo_is_refused_r7() {
    let f = fixture();
    symlink("/", f.repo.join("root")).unwrap();
    symlink("../mnt", f.repo.join("allmnt")).unwrap();
    symlink("..", f.repo.join("up")).unwrap();
    git(&f.repo, &["add", "root", "allmnt", "up"]);
    let (addr, nonce, serial) = serve(widened(&f).await).await;
    let share_movie = f.share.join("results/r1/movie.mp4");
    let under_root = share_movie.strip_prefix("/").unwrap().display().to_string();
    for (l, rest) in [("root", under_root.as_str()), ("allmnt", "share/results/r1/movie.mp4"), ("up", "mnt/share/results/r1/movie.mp4")] {
        let (st, _, body) = get(addr, &format!("/{nonce}/{l}/{rest}"), &[]).await;
        assert_eq!(
            (st, body_text(&body)),
            (403, format!("refused: {l} is a tracked link to /, to a folder holding the home folder or to a folder holding this repository, and such a link is never followed")),
            "{l}"
        );
    }
    let asked = "up/outside/secret.txt";
    let (st, _, body) = get(addr, &format!("/{nonce}/{asked}"), &[]).await;
    assert_eq!((st, body_text(&body)), (403, r3(asked, "up")));
    remove_root(serial);
}

/// A mount folder given through a symlink (an OS whose `/mnt` is a link) matches its canonical path.
#[tokio::test]
async fn a_mount_folder_given_through_a_symlink_matches_its_canonical_path() {
    let f = fixture();
    let alias = f.base.join("mnt-alias");
    symlink(f.base.join("mnt"), &alias).unwrap();
    let mut site = widened(&f).await;
    site.bounds = Bounds::new(&[&alias], Some(f.base.join("mnt/home/u").as_path()));
    let (addr, nonce, serial) = serve(site).await;
    let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &[]).await;
    assert_eq!(st, 200);
    assert_eq!(body.len(), MOVIE_LEN);
    remove_root(serial);
}

/// Git is run everywhere but on a Mac with no Command Line Tools, where `/usr/bin/git` is an install prompt.
#[test]
fn git_runs_unless_it_is_the_macos_install_stub() {
    assert!(git_usable(false, false));
    assert!(git_usable(true, true));
    assert!(!git_usable(true, false));
}

/// Where git cannot run, a tracked link is refused with words that say so, not "git does not track".
#[tokio::test]
async fn a_link_where_git_cannot_run_is_refused_with_the_install_words() {
    let f = fixture();
    let mut site = widened(&f).await;
    site.git.as_mut().unwrap().usable = false;
    let (addr, nonce, serial) = serve(site).await;
    let (st, _, body) = get(addr, &format!("/{nonce}/{MOVIE}"), &[]).await;
    assert_eq!(st, 403);
    let body = body_text(&body);
    assert!(body.contains("data is a link, and linked data needs git; on macOS install the Command Line Tools, then restart the daemon"), "{body}");
    assert!(!body.contains("does not track"), "{body}");
    remove_root(serial);
}

/// With git unusable the repo top comes from a walk up for `.git`, and the URL space widens to it only within the
/// workspace root, never above it and not when the workspace is the site folder.
#[tokio::test]
async fn without_git_the_walk_up_for_dot_git_widens_only_within_the_workspace() {
    let f = fixture();
    let site_dir = f.repo.join("site");
    for (ws, widened) in [(&f.repo, true), (&f.base, true), (&site_dir, false)] {
        let mut site = Site::plain(site_dir.clone());
        site.attach_git(ws, true, false);
        let git = site.git.as_ref().expect("a repo found by the walk");
        assert!(!git.usable);
        assert_eq!(git.top, f.repo);
        assert_eq!(site.url_root, if widened { f.repo.clone() } else { site_dir.clone() }, "workspace {}", ws.display());
        assert!(site.git.as_ref().unwrap().links(&site.url_root).is_empty());
    }
}
