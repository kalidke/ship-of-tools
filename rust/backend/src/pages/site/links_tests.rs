//! Tests of the follow rule: linked data roots, R0-R6 refusals and the stat-keyed caches.

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
