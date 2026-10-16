#![cfg(unix)]
//! A window's pages outlive each of its connections, through a real `sotd`. A page `docs.open` granted a window keeps
//! serving when the window redials (its hello's host and `instance`, which every connection of one window shares), a
//! reopen still ends the page it replaces, and another window's opens never take it. A window's pages end when the
//! daemon forgets the window, and a connection whose hello names no window keeps its pages only while it lasts. A pool
//! port goes to a new open from a window with no live connection only when every port is held, and that window's old
//! link is then refused (ADR 0029, 0.6.6 amendment).

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

use std::path::{Path, PathBuf};

use serde_json::json;
use sot_protocol::{op, HelloReq};
use support::{call, connect_and_hello, poll_until, try_connect, Conn, Env, BOUND};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How many windows with no live connection the daemon remembers (`AWAY`, `rust/backend/src/clients_window.rs`).
const AWAY: usize = 64;

/// A daemon whose page servers bind where the OS assigns, so a page server another daemon holds on this computer
/// costs no bind retries.
fn daemon(tag: &str) -> Env {
    let env = Env::new(tag);
    env.spawn_sotd_with_env(&[("SOT_DOCS_PORT", "0"), ("SOT_VIDEO_PORT", "0")]);
    env
}

/// A connection whose hello says `role`, and `instance` when it names one: a window's hello is role `fe` with the
/// window's address and `instance` (`rust/frontend/src/net/transport/hello.rs`).
async fn connect(env: &Env, client_id: &str, role: &str, instance: Option<&str>) -> Conn {
    let stream = poll_until(|| try_connect(&env.socket_path), BOUND, "sotd's socket to accept a connection").await;
    let mut conn = tokio::io::BufReader::new(stream);
    let hello = HelloReq {
        instance: instance.map(Into::into),
        name: Some("fe@host-a".to_string()),
        ..HelloReq::this_process(client_id, role, Some("host-a".to_string())).expect("this process's account")
    };
    let reply = call(&mut conn, 1, op::HELLO, serde_json::to_value(&hello).expect("a hello")).await;
    assert!(reply.payload.get("error").is_none(), "hello refused: {:?}", reply.payload);
    conn
}

async fn window(env: &Env, client_id: &str, instance: &str) -> Conn {
    connect(env, client_id, "fe", Some(instance)).await
}

/// `docs.open` of `path` on `conn`: the reply's payload.
async fn open_reply(conn: &mut Conn, id: u64, path: &Path) -> serde_json::Value {
    call(conn, id, op::DOCS_OPEN, json!({ "path": path })).await.payload
}

/// `docs.open` of `path` on `conn`: the page's URL.
async fn open(conn: &mut Conn, id: u64, path: &Path) -> String {
    let reply = open_reply(conn, id, path).await;
    let url = reply["url"].as_str().unwrap_or_else(|| panic!("docs.open {}: {reply}", path.display()));
    url.to_string()
}

/// The status a browser is answered for `url` (`http://127.0.0.1:<port>/<path>`), on a connection of its own.
async fn status(url: &str) -> u16 {
    let rest = url.strip_prefix("http://127.0.0.1:").expect("a loopback page URL");
    let (port, path) = rest.split_once('/').expect("a path");
    let port: u16 = port.parse().expect("a port");
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.expect("the page server");
    let request = format!("GET /{path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("a request");
    let mut reply = Vec::new();
    tokio::time::timeout(BOUND, stream.read_to_end(&mut reply)).await.expect("a reply within the bound").expect("read");
    let reply = String::from_utf8_lossy(&reply);
    reply.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("no status in {reply:?}"))
}

/// Until the daemon has seen every connection of `client_id` close: `version.query` lists none.
async fn until_gone(env: &Env, client_id: &str) {
    let listed = || async {
        let (mut conn, id) = connect_and_hello(&env.socket_path).await;
        let reply = call(&mut conn, id, op::VERSION_QUERY, json!({})).await;
        let clients = reply.payload["clients"].as_array().cloned().unwrap_or_default();
        (!clients.iter().any(|c| c["client_id"] == client_id)).then_some(())
    };
    poll_until(listed, BOUND, "the daemon to see the connection close").await;
}

/// Until `url` is answered `want`.
async fn until_status(url: &str, want: u16, what: &str) {
    poll_until(|| async { (status(url).await == want).then_some(()) }, BOUND, what).await;
}

/// The pages under the daemon's own row: a page with page-relative links (served by the prefix server), another, and
/// a site whose entry page uses root-relative links (served on a pool port).
fn pages(env: &Env) -> (PathBuf, PathBuf, PathBuf) {
    let root = env.daemon_project_root.join("pages");
    let write = |rel: &str, body: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a folder")).expect("mkdir");
        std::fs::write(&path, body).expect("write a page");
        path
    };
    let deck = write("deck/slides.html", "<p>slides</p>");
    let notes = write("notes/notes.html", "<p>notes</p>");
    let rooted = write("rooted/index.html", "<link rel=\"stylesheet\" href=\"/style.css\"><p>rooted</p>");
    write("rooted/style.css", "p {}");
    (deck, notes, rooted)
}

/// The field red (SITE-SURVIVES-REDIAL): a deck opened from a window was answered 404 after that window's link dropped
/// and it redialed. A redial is the same window, so its pages serve on; another window's open takes none of them; a
/// reopen still ends the page it replaces, so the old link is refused, never served the new page.
#[tokio::test]
async fn a_window_that_redials_keeps_its_pages() {
    let env = daemon("sitewin");
    let (deck, notes, rooted) = pages(&env);
    let mut a = window(&env, "client-w", "window-i").await;
    let page = open(&mut a, 2, &deck).await;
    let pool = open(&mut a, 3, &rooted).await;
    assert_eq!((status(&page).await, status(&pool).await), (200, 200), "the window's pages serve: {page} {pool}");
    drop(a); // the window's link drops; the same window redials
    until_gone(&env, "client-w").await;
    let mut b = window(&env, "client-w", "window-i").await;
    let after = (status(&page).await, status(&pool).await);
    assert_eq!(after, (200, 200), "the pages the window opened before its redial: {page} {pool}");
    let mut j = window(&env, "client-j", "window-j").await;
    let theirs = open(&mut j, 2, &notes).await;
    assert_eq!((status(&page).await, status(&theirs).await), (200, 200), "another window's open takes none of them");
    let page_again = open(&mut b, 2, &deck).await;
    let pool_again = open(&mut b, 3, &rooted).await;
    assert_eq!((status(&page).await, status(&page_again).await), (404, 200), "a reopen ends the page it replaces");
    let pools = (status(&pool).await, status(&pool_again).await);
    assert_eq!(pools, (403, 200), "a reopened pool site refuses its old secret");
    drop((b, j));
    env.kill_daemon_bounded().await;
}

/// A window's pages end when the daemon forgets the window: past `AWAY` windows with no live connection, the one away
/// longest. A connection whose hello names no window keeps its page only while it lasts.
#[tokio::test]
async fn pages_end_with_a_forgotten_window_or_a_connection_of_no_window() {
    let env = daemon("sitegone");
    let (deck, notes, _) = pages(&env);
    let mut cli = connect(&env, "client-cli", "cli", None).await;
    let theirs = open(&mut cli, 2, &notes).await;
    assert_eq!(status(&theirs).await, 200, "the connection's page serves");
    drop(cli);
    until_status(&theirs, 404, "a page of a connection of no window to end with it").await;
    let mut a = window(&env, "client-w", "window-i").await;
    let page = open(&mut a, 2, &deck).await;
    drop(a);
    until_gone(&env, "client-w").await;
    for i in 1..=AWAY {
        drop(window(&env, &format!("client-{i}"), &format!("away-{i}")).await);
        until_gone(&env, &format!("client-{i}")).await;
    }
    until_status(&page, 404, "the page of the window away longest to end when it is forgotten").await;
    env.kill_daemon_bounded().await;
}

/// The pool's ports are few (four), and a window away holds its own. When every port is held, a new open takes the port
/// of a window with no live connection, whose old link is then refused; a port a live window holds is never taken.
#[tokio::test]
async fn a_full_pool_takes_a_port_only_from_a_window_away() {
    let env = daemon("sitepool");
    let (_, _, rooted) = pages(&env);
    let mut held = Vec::new();
    for i in 1..=4 {
        let mut w = window(&env, &format!("client-{i}"), &format!("window-{i}")).await;
        let url = open(&mut w, 2, &rooted).await;
        held.push((w, url));
    }
    let (away, away_url) = held.remove(0);
    drop(away);
    until_gone(&env, "client-1").await;
    let mut fifth = window(&env, "client-5", "window-5").await;
    let taken = open(&mut fifth, 2, &rooted).await;
    assert_eq!((status(&away_url).await, status(&taken).await), (403, 200), "the port of the window away: {taken}");
    for (_, url) in &held {
        assert_eq!(status(url).await, 200, "a live window's pool site: {url}");
    }
    let mut sixth = window(&env, "client-6", "window-6").await;
    let busy = open_reply(&mut sixth, 2, &rooted).await;
    assert_eq!(busy["code"], "root_relative_pool_busy", "every port is held by a live window: {busy}");
    drop((held, fifth, sixth));
    env.kill_daemon_bounded().await;
}
