#![cfg(unix)]
//! Which folder a page `docs.open` serves, through a real `sotd` whose home is its project root, the shape of a
//! daemon started with `--project-root $HOME`. A page, an index page and a folder opened directly are served from the
//! site they belong to, so the assets their relative links name load. The walk to the site never leaves the page's
//! repository and never takes in the home folder, and a page directly in it is refused (ADR 0024, amendment
//! 2026-10-16).

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use std::path::Path;

use serde_json::json;
use sot_protocol::{op, HelloReq};
use support::{call, poll_until, try_connect, Conn, Env, BOUND};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A daemon whose home is its project root, with page servers bound where the OS assigns.
fn daemon(tag: &str) -> Env {
    let env = Env::new(tag);
    let home = env.daemon_project_root.to_string_lossy().into_owned();
    env.spawn_sotd_with_env(&[("SOT_DOCS_PORT", "0"), ("SOT_VIDEO_PORT", "0"), ("HOME", &home)]);
    env
}

/// A window's connection: its hello names the window's host and `instance`.
async fn window(env: &Env) -> Conn {
    let stream = poll_until(|| try_connect(&env.socket_path), BOUND, "sotd's socket to accept a connection").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let hello = HelloReq {
        instance: Some("window-i".into()),
        name: Some("fe@host-a".to_string()),
        ..HelloReq::this_process("client-w", "fe", Some("host-a".to_string())).expect("this process's account")
    };
    let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).expect("a hello")).await;
    assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
    conn
}

/// `docs.open` of `path`: the reply's payload.
async fn open_reply(conn: &mut Conn, id: u64, path: &Path) -> serde_json::Value {
    call(conn, id, op::DOCS_OPEN, json!({ "path": path })).await.payload
}

/// `docs.open` of `path`: the page's URL.
async fn open(conn: &mut Conn, id: u64, path: &Path) -> String {
    let reply = open_reply(conn, id, path).await;
    let url = reply["url"].as_str().unwrap_or_else(|| panic!("docs.open {}: {reply}", path.display()));
    url.to_string()
}

/// The status a browser is answered for `link` on the page at `page`: `link` resolves against the page's URL as a
/// browser resolves it, a `..` stopping at the origin's `/`.
async fn status(page: &str, link: &str) -> u16 {
    let rest = page.strip_prefix("http://127.0.0.1:").expect("a loopback page URL");
    let (port, path) = rest.split_once('/').expect("a path");
    let port: u16 = port.parse().expect("a port");
    let mut parts: Vec<&str> = path.split('/').collect();
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
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("the page server");
    let request = format!("GET /{} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n", parts.join("/"));
    stream.write_all(request.as_bytes()).await.expect("a request");
    let mut reply = Vec::new();
    tokio::time::timeout(BOUND, stream.read_to_end(&mut reply)).await.expect("a reply within the bound").expect("read");
    let reply = String::from_utf8_lossy(&reply);
    reply.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("no status in {reply:?}"))
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("a folder")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

/// A git repository at `dir`.
fn repo(dir: &Path) {
    std::fs::create_dir_all(dir).expect("mkdir");
    let mut git = std::process::Command::new("git");
    git.args(["init", "-q"]).arg(dir).env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
    sot_log::test_wait::run_within(&mut git, BOUND).ok();
}

/// A project log in a repository: one site with index pages inside it (`journal/`, each campaign's) and a folder with
/// none between (`experiments/`).
fn project_log(env: &Env) -> std::path::PathBuf {
    repo(&env.daemon_project_root.join("code/project"));
    let log = env.daemon_project_root.join("code/project/project-log");
    let css = |up: &str| {
        format!("<link rel=\"stylesheet\" href=\"{up}assets/site.css\"><script src=\"{up}assets/site.js\">")
    };
    write(&log.join("index.html"), &css(""));
    write(&log.join("assets/site.css"), "p {}");
    write(&log.join("assets/site.js"), "");
    write(&log.join("journal/index.html"), &css("../"));
    write(&log.join("experiments/camp/index.html"), &css("../../"));
    write(&log.join("experiments/camp/run.html"), &css("../../"));
    log
}

/// The field red: a campaign page opened directly was served from its campaign folder, whose index ended the walk
/// below the index-less `experiments/`, and lost the log's CSS and scripts. It is served from the log now.
#[tokio::test]
async fn a_campaign_page_opened_directly_loads_the_log_assets() {
    let env = daemon("siteroot-camp");
    let log = project_log(&env);
    let mut w = window(&env).await;
    let run = open(&mut w, 2, &log.join("experiments/camp/run.html")).await;
    let assets = (status(&run, "../../assets/site.css").await, status(&run, "../../assets/site.js").await);
    assert_eq!(assets, (200, 200), "a campaign page's assets: {run}");
    drop(w);
    env.kill_daemon_bounded().await;
}

/// An index page opened directly, or its folder, was always served from that folder. Each is served from the site it
/// belongs to, at the folder's place there; the front page is served as before.
#[tokio::test]
async fn an_index_page_opened_directly_loads_its_site_assets() {
    let env = daemon("siteroot-index");
    let log = project_log(&env);
    let mut w = window(&env).await;
    let camp = open(&mut w, 2, &log.join("experiments/camp/index.html")).await;
    assert_eq!(status(&camp, "../../assets/site.css").await, 200, "a campaign's index page: {camp}");
    let journal = open(&mut w, 3, &log.join("journal/index.html")).await;
    assert_eq!(status(&journal, "../assets/site.css").await, 200, "the journal's index page: {journal}");
    let front = open(&mut w, 4, &log.join("index.html")).await;
    assert_eq!(status(&front, "assets/site.css").await, 200, "the front page: {front}");
    let folder = open(&mut w, 5, &log.join("journal")).await;
    assert_eq!(status(&folder, "../assets/site.css").await, 200, "the journal's folder: {folder}");
    drop(w);
    env.kill_daemon_bounded().await;
}

/// A report in a repository with no index page of its own was served from the first folder above it holding one, here
/// a folder of several repositories, so its page could read another repository's files. The walk ends at the top of
/// the page's repository.
#[tokio::test]
async fn a_site_never_leaves_its_repository() {
    let env = daemon("siteroot-repo");
    let code = env.daemon_project_root.join("code");
    write(&code.join("index.html"), "<p>code</p>");
    write(&code.join("other/notes.txt"), "notes");
    repo(&code.join("report"));
    write(&code.join("report/results/run7/report.html"), "<img src=\"f.png\">");
    write(&code.join("report/results/run7/f.png"), "png");
    let mut w = window(&env).await;
    let report = open(&mut w, 2, &code.join("report/results/run7/report.html")).await;
    assert_eq!(status(&report, "f.png").await, 200, "the report's own figure: {report}");
    assert_eq!(status(&report, "../../../other/notes.txt").await, 404, "a file outside its repository: {report}");
    drop(w);
    env.kill_daemon_bounded().await;
}

/// A page with no index between it and an index page in the home folder was served from the home folder, so its page
/// could read the ssh keys there. The walk to its site stops below the home folder.
#[tokio::test]
async fn a_site_never_takes_in_the_home_folder() {
    let env = daemon("siteroot-home");
    let home = &env.daemon_project_root;
    write(&home.join("index.html"), "<p>home</p>");
    write(&home.join(".ssh/id_test"), "key");
    write(&home.join("downloads/bundle/page.html"), "<p>page</p>");
    let mut w = window(&env).await;
    let page = open(&mut w, 2, &home.join("downloads/bundle/page.html")).await;
    assert_eq!(status(&page, "page.html").await, 200, "the page itself: {page}");
    assert_eq!(status(&page, "../../.ssh/id_test").await, 404, "a file in the home folder: {page}");
    drop(w);
    env.kill_daemon_bounded().await;
}

/// A page directly in the home folder was served from the home folder, keys and all. No site root holds the home
/// folder, so the open is refused and says why.
#[tokio::test]
async fn a_page_in_the_home_folder_is_refused() {
    let env = daemon("siteroot-homepage");
    let home = &env.daemon_project_root;
    write(&home.join(".ssh/id_test"), "key");
    write(&home.join("report.html"), "<p>report</p>");
    let mut w = window(&env).await;
    let reply = open_reply(&mut w, 2, &home.join("report.html")).await;
    assert_eq!(reply["code"], "home_folder", "a page in the home folder: {reply}");
    drop(w);
    env.kill_daemon_bounded().await;
}
