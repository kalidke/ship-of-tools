//! Page verbs through a real daemon, scratch registry, docs.open and HTTP listener.
//! No agent is started: the target is an inert row with a declared handle.

mod support;

use serde_json::{json, Value};
use sot_protocol::{codec, op, Frame, HelloReq, Kind};
use std::path::Path;
use std::time::Duration;
use support::{call, Conn, Env, TEST_STATE_HOST};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const BOUND: Duration = Duration::from_secs(30);
const QUIET: Duration = Duration::from_secs(2);
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const ROW: &str = "page-row";
const HANDLE: &str = "row-here";

async fn window(env: &Env, instance: &str) -> Conn {
    let ready = tokio::time::timeout(BOUND, async {
        loop {
            if let Some(stream) = support::try_connect(&env.socket_path).await {
                return Ok(stream);
            }
            let status = env.daemon.borrow_mut().as_mut().expect("tracked page daemon")
                .try_wait().expect("check page daemon status");
            if let Some(status) = status {
                return Err(status);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await;
    let stream = match ready {
        Ok(Ok(stream)) => stream,
        Ok(Err(status)) => panic!("page daemon exited before binding: {status}\n{}", env.daemon_log_since(0)),
        Err(_) => panic!("timed out waiting for page daemon\n{}", env.daemon_log_since(0)),
    };
    let mut conn = tokio::io::BufReader::new(stream);
    // Both windows deliberately share a name. Only the connection serial distinguishes them.
    let hello = HelloReq {
        instance: Some(instance.into()), name: Some("fe@test-window".into()),
        ..HelloReq::this_process("page-test", "fe", Some("test-window".into())).unwrap()
    };
    let res = call(&mut conn, 1, op::HELLO, serde_json::to_value(hello).unwrap()).await;
    assert!(res.payload.get("error").is_none(), "{res:?}");
    conn
}

async fn setup(tag: &str) -> (Env, Conn) {
    setup_with_slug(tag, ROW).await
}

async fn setup_with_slug(tag: &str, slug: &str) -> (Env, Conn) {
    let env = Env::new(tag);
    env.seed_capsule_toml(ROW, slug, &env.workspace_project_root, "none");
    // The page belongs to the default workspace; its actions target a different row.
    let site = env.daemon_project_root.join("queue");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("index.html"), include_str!("fixtures/page_channel/index.html")).unwrap();
    std::fs::write(env.workspace_project_root.join("decision.html"), "<p>Decision</p>").unwrap();
    env.spawn_sotd_with_env(&[("SOT_DOCS_PORT", "0"), ("SOT_VIDEO_PORT", "0")]);
    let mut conn = window(&env, "window-a").await;
    let joined = call(&mut conn, 2, op::AGENT_JOIN, json!({"workspace_id": ROW, "handle": HANDLE})).await;
    assert_eq!(joined.payload["ok"], true, "{joined:?}");
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "workspace_id": ROW, "state": "idle"}));
    (env, conn)
}

fn set_entry(env: &Env, handle: &str, entry: Value) {
    support::write_registry(&env.comm_root, |doc| { doc["agents"][handle] = entry; });
}

#[derive(Clone)]
struct SiteUrl { host: String, target: String }

impl SiteUrl {
    fn parse(url: &str) -> Self {
        let (host, path) = url.strip_prefix("http://").unwrap().split_once('/').unwrap();
        Self { host: host.into(), target: format!("/{path}") }
    }

    async fn connect(&self) -> TcpStream {
        tokio::time::timeout(BOUND, TcpStream::connect(self.host.as_str())).await.unwrap().unwrap()
    }

    fn foreign_origin(&self) -> String {
        let port: u16 = self.host.rsplit_once(':').unwrap().1.parse().unwrap();
        format!("http://127.0.0.1:{}", if port == 1 { 2 } else { 1 })
    }

    async fn post(&self, body: Value) -> Reply {
        self.raw("POST", Some(&format!("http://{}", self.host)), "", &body.to_string()).await
    }

    async fn raw(&self, method: &str, origin: Option<&str>, extra: &str, body: &str) -> Reply {
        let mut stream = self.connect().await;
        let origin = origin.map(|s| format!("Origin: {s}\r\n")).unwrap_or_default();
        let head = format!(
            "{method} {} HTTP/1.1\r\nHost: {}\r\n{origin}{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.target, self.host, body.len(),
        );
        exchange(&mut stream, &head, method == "HEAD").await
    }
}

struct Reply { status: u16, headers: String, body: Vec<u8> }

impl Reply {
    fn json(&self) -> Value {
        assert!(self.headers.contains("Cache-Control: no-store\r\n"), "{}", self.headers);
        assert!(self.headers.contains("Content-Type: application/json"), "{}", self.headers);
        serde_json::from_slice(&self.body).expect("JSON reply")
    }
    fn code(&self, status: u16, code: &str) {
        assert_eq!(self.status, status, "{}", String::from_utf8_lossy(&self.body));
        let body = self.json();
        assert_eq!(body["code"], code);
        assert!(!body["error"].as_str().unwrap().is_empty());
    }
}

async fn exchange<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(stream: &mut S, request: &str, head_only: bool) -> Reply {
    tokio::time::timeout(BOUND, async {
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut head = Vec::new();
        let mut byte = [0; 1];
        while !head.ends_with(b"\r\n\r\n") {
            assert_eq!(stream.read(&mut byte).await.unwrap(), 1, "closed mid-head");
            head.push(byte[0]);
            assert!(head.len() < 16 * 1024);
        }
        let headers = String::from_utf8(head).unwrap();
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        let len: usize = headers.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
        let mut body = vec![0; if head_only { 0 } else { len }];
        stream.read_exact(&mut body).await.unwrap();
        Reply { status, headers, body }
    }).await.expect("whole HTTP exchange within bound")
}

async fn open(conn: &mut Conn, path: &Path) -> SiteUrl {
    let res = call(conn, 10, op::DOCS_OPEN, json!({"path": path})).await;
    assert!(res.payload.get("error").is_none(), "{res:?}");
    SiteUrl::parse(res.payload["url"].as_str().unwrap())
}

async fn open_scoped(conn: &mut Conn, path: &Path, direct: Value) -> SiteUrl {
    let res = call(conn, 10, op::DOCS_OPEN, json!({"path": path, "direct": direct})).await;
    assert!(res.payload.get("error").is_none(), "{res:?}");
    SiteUrl::parse(res.payload["url"].as_str().unwrap())
}

async fn reply_scope(site: &SiteUrl) -> Value {
    let res = site.post(json!({"verb": "states"})).await;
    assert_eq!(res.status, 200, "{}", String::from_utf8_lossy(&res.body));
    res.json()["reply_scope"].clone()
}

#[tokio::test]
async fn states_reports_the_entry_scope_with_a_canonical_row_id() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup_with_slug("page-scopes", "direct-alias").await;
    let ordinary = queue(&env, &mut a).await;
    assert_eq!(reply_scope(&ordinary).await, json!({"scope": "none"}), "an old frontend sends only path");
    let all = open_scoped(&mut a, &env.daemon_project_root.join("queue/index.html"), json!({"scope": "all"})).await;
    assert_eq!(reply_scope(&all).await, json!({"scope": "all"}));
    let row = open_scoped(&mut a, &env.workspace_project_root.join("decision.html"),
        json!({"scope": "row", "workspace_id": "direct-alias"})).await;
    assert_eq!(reply_scope(&row).await, json!({"scope": "row", "workspace_id": ROW}));
    std::fs::write(env.workspace_project_root.join("index.html"), "<p>Row index</p>").unwrap();
    let directory = open_scoped(&mut a, &env.workspace_project_root, json!({"scope": "row", "workspace_id": ROW})).await;
    assert_eq!(reply_scope(&directory).await, json!({"scope": "row", "workspace_id": ROW}));
    assert_eq!(directory.raw("GET", None, "", "").await.status, 200);
}

#[tokio::test]
async fn a_direct_row_open_refuses_a_page_in_another_known_workspace() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-scope-outside").await;
    let outside = env.daemon_project_root.join("queue/index.html");
    let refused = call(&mut a, 10, op::DOCS_OPEN,
        json!({"path": outside, "direct": {"scope": "row", "workspace_id": ROW}})).await;
    assert_eq!(refused.payload["code"], "page_outside", "{refused:?}");
    assert!(!refused.payload["error"].as_str().unwrap().is_empty());
    assert!(refused.payload.get("url").is_none(), "refusal must not downgrade to an ordinary open");
    #[cfg(unix)]
    {
        let link = env.workspace_project_root.join("escape.html");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let refused = call(&mut a, 11, op::DOCS_OPEN,
            json!({"path": link, "direct": {"scope": "row", "workspace_id": ROW}})).await;
        assert_eq!(refused.payload["code"], "page_outside");
        assert!(refused.payload.get("url").is_none());
        let directory = env.workspace_project_root.join("escaped-index");
        std::fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(&outside, directory.join("index.html")).unwrap();
        let refused = call(&mut a, 11, op::DOCS_OPEN,
            json!({"path": directory, "direct": {"scope": "row", "workspace_id": ROW}})).await;
        assert_eq!(refused.payload["code"], "page_outside");
    }
    let missing = call(&mut a, 12, op::DOCS_OPEN,
        json!({"path": outside, "direct": {"scope": "row", "workspace_id": "unregistered-row"}})).await;
    assert_eq!(missing.payload["code"], "unknown_workspace");
}

#[tokio::test]
async fn the_open_verb_always_grants_the_targets_row_and_never_all() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-open-scope").await;
    let page = env.workspace_project_root.join("decision.html");
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "workspace_id": ROW, "state": "blocked",
        "question": format!("Land it? Page: {}", page.display())}));
    for scope in [json!({"scope": "none"}), json!({"scope": "row", "workspace_id": ROW}), json!({"scope": "all"})] {
        let opener = open_scoped(&mut a, &page, scope).await;
        let offered = opener.post(json!({"verb": "open", "handle": HANDLE, "direct": {"scope": "all"}})).await;
        assert_eq!(offered.status, 202);
        let args = command(&mut a, "docs").await;
        // Neither a forged POST field nor a wider redemption request may upgrade the already-made grant.
        let res = call(&mut a, 11, op::DOCS_OPEN, json!({"path": args["path"], "direct": {"scope": "all"}})).await;
        let decision = SiteUrl::parse(res.payload["url"].as_str().unwrap());
        assert_eq!(reply_scope(&decision).await, json!({"scope": "row", "workspace_id": ROW}));
        assert_eq!(decision.raw("GET", None, "", "").await.status, 200);
    }
}

#[tokio::test]
async fn navigation_from_a_direct_entry_gets_an_ordinary_grant_in_both_servers() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-scope-navigation").await;
    for pool in [false, true] {
        let folder = env.workspace_project_root.join(if pool { "pool" } else { "prefix" });
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("index.html"), if pool { "<script src=\"/a.js\"></script>" } else { "<p>Entry</p>" }).unwrap();
        std::fs::write(folder.join("other page.html"), "<p>Other document</p>").unwrap();
        std::fs::write(folder.join("a.js"), "// asset").unwrap();
        for scope in [json!({"scope": "row", "workspace_id": ROW}), json!({"scope": "all"})] {
            let direct = open_scoped(&mut a, &folder.join("index.html"), scope.clone()).await;
            assert_eq!(reply_scope(&direct).await, scope);
            let entry_file = direct.raw("GET", None, "", "").await;
            assert_eq!(entry_file.status, 200);
            let (path, query) = direct.target.split_once('?').unwrap_or((&direct.target, ""));
            let parent = path.rsplit_once('/').unwrap().0;
            let suffix = if query.is_empty() { String::new() } else { format!("?{query}") };
            let other = SiteUrl { target: format!("{parent}/other%20page.html{suffix}"), ..direct.clone() };
            if pool {
                let cookie = entry_file.headers.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap();
                let bare = SiteUrl { target: format!("{parent}/other%20page.html?view=waiting"), ..direct.clone() };
                let redirected = bare.raw("GET", None, &format!("Cookie: {cookie}\r\n"), "").await;
                assert_eq!(redirected.status, 302, "a browser's relative navigation carries its cookie, not the entry's query");
                let target = redirected.headers.lines().find_map(|l| l.strip_prefix("Location: ")).unwrap().to_string();
                assert!(target.contains("view=waiting&secret="));
                assert_eq!(reply_scope(&SiteUrl { target, ..direct.clone() }).await, json!({"scope": "none"}));
            }
            let redirected = other.raw("GET", None, "", "").await;
            assert_eq!(redirected.status, 302, "another HTML document must exchange capabilities");
            let target = redirected.headers.lines().find_map(|l| l.strip_prefix("Location: ")).unwrap().to_string();
            assert_ne!(target, other.target);
            let ordinary = SiteUrl { target, ..direct.clone() };
            let file = ordinary.raw("GET", None, "", "").await;
            assert_eq!(file.status, 200);
            assert_eq!(file.body, b"<p>Other document</p>");
            assert_eq!(reply_scope(&ordinary).await, json!({"scope": "none"}));
            let back = SiteUrl { target: ordinary.target.replace("other%20page.html", "index.html"), ..ordinary.clone() };
            assert_eq!(back.raw("GET", None, "", "").await.status, 200);
            assert_eq!(reply_scope(&back).await, json!({"scope": "none"}), "navigation back cannot upgrade an ordinary grant");
            assert_eq!(reply_scope(&direct).await, scope, "the original entry keeps its own grant");
            if pool {
                let cookie = file.headers.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap();
                let asset = SiteUrl { target: "/a.js".into(), ..ordinary.clone() };
                assert_eq!(asset.raw("GET", None, &format!("Cookie: {cookie}\r\n"), "").await.status, 200,
                    "ordinary navigation retains root-relative assets on the same pool origin");
                let replacement = open_scoped(&mut a, &folder.join("index.html"), scope.clone()).await;
                ordinary.post(json!({"verb": "states"})).await.code(403, "forbidden");
                direct.post(json!({"verb": "states"})).await.code(403, "forbidden");
                assert_eq!(reply_scope(&replacement).await, scope);
            }
        }
    }
}

#[tokio::test]
async fn a_post_cannot_change_scope_and_an_unscoped_reply_is_refused() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-post-scope").await;
    let page = env.workspace_project_root.join("decision.html");
    for scope in [json!({"scope": "none"}), json!({"scope": "row", "workspace_id": ROW}), json!({"scope": "all"})] {
        let site = open_scoped(&mut a, &page, scope.clone()).await;
        let forged = json!({"scope": "all", "workspace_id": "another-row"});
        let res = site.post(json!({"verb": "states", "direct": forged, "reply_scope": forged})).await;
        assert_eq!(res.status, 200);
        assert_eq!(res.json()["reply_scope"], scope);
        let reply = site.post(json!({"verb": "reply", "handle": HANDLE, "text": "approve", "direct": forged})).await;
        if scope == json!({"scope": "none"}) {
            // No reply waits on the window's bar: an unscoped page is told why, and nothing is offered or typed.
            reply.code(403, "no_reply_scope");
            no_command(&mut a).await;
        } else {
            assert_eq!(reply.status, 200, "a scoped reply is rows' to deliver: {:?}", reply.json());
            assert!(reply.json()["receipt"]["outcome"].is_string(), "{:?}", reply.json());
            no_command(&mut a).await;
        }
        assert_eq!(reply_scope(&site).await, scope);
        let elsewhere = SiteUrl { target: site.target.replace("decision.html", "no-file-needed"), ..site.clone() };
        assert_eq!(reply_scope(&elsewhere).await, json!({"scope": "none"}), "a POST path beneath the nonce does not inherit entry authority");
    }
}

async fn queue(env: &Env, conn: &mut Conn) -> SiteUrl {
    open(conn, &env.daemon_project_root.join("queue/index.html")).await
}

async fn command(conn: &mut Conn, cmd: &str) -> Value {
    tokio::time::timeout(BOUND, async {
        loop {
            let (frame, _) = codec::read_frame(conn).await.expect("window frame");
            if frame.kind == Kind::Evt && frame.op == op::FE_COMMAND {
                assert_eq!(frame.payload["cmd"], cmd);
                assert_eq!(frame.payload["target"], "fe@test-window");
                return frame.payload["args"].clone();
            }
        }
    }).await.expect("command for page's window")
}

async fn no_command(conn: &mut Conn) {
    let got = tokio::time::timeout(QUIET, async {
        loop {
            let (frame, _) = codec::read_frame(conn).await.expect("window still connected");
            if frame.kind == Kind::Evt && frame.op == op::FE_COMMAND { return frame; }
        }
    }).await;
    assert!(got.is_err(), "unexpected command: {got:?}");
}

#[tokio::test]
async fn a_go_to_reaches_only_the_window_that_opened_the_page() {
    let _serial = SERIAL.lock().await;
    let (env, mut old) = setup("page-route").await;
    let site_a = queue(&env, &mut old).await;
    let mut b = window(&env, "window-b").await;
    let site_b = queue(&env, &mut b).await;
    let mut newest = window(&env, "window-a").await;
    assert_eq!(site_a.post(json!({"verb": "goto", "handle": HANDLE})).await.status, 202);
    assert_eq!(command(&mut newest, "goto_workspace").await, json!({"workspace": ROW}));
    tokio::join!(no_command(&mut old), no_command(&mut b));
    assert_eq!(site_b.post(json!({"verb": "goto", "handle": HANDLE})).await.status, 202);
    assert_eq!(command(&mut b, "goto_workspace").await, json!({"workspace": ROW}));
    tokio::join!(no_command(&mut old), no_command(&mut newest));
}

#[tokio::test]
async fn a_verb_without_its_sites_capability_or_from_another_origin_is_refused() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-auth").await;
    let site = queue(&env, &mut a).await;
    let body = json!({"verb": "goto", "handle": HANDLE});
    assert_eq!(site.post(body.clone()).await.status, 202);
    command(&mut a, "goto_workspace").await;
    let unknown = SiteUrl { target: "/unknown/index.html".into(), ..site.clone() };
    assert_eq!(unknown.post(body.clone()).await.status, 404);
    let foreign = site.foreign_origin();
    for origin in [None, Some("null"), Some(foreign.as_str())] {
        site.raw("POST", origin, "", &body.to_string()).await.code(403, "forbidden");
    }
    // A pool site's cookie travels across loopback ports: Origin still has to match.
    let pool_root = env.workspace_project_root.join("pool");
    std::fs::create_dir_all(&pool_root).unwrap();
    std::fs::write(pool_root.join("index.html"), "<script src=\"/a.js\"></script>").unwrap();
    std::fs::write(pool_root.join("a.js"), "").unwrap();
    let pool = open(&mut a, &pool_root.join("index.html")).await;
    let first = pool.raw("GET", None, "", "").await;
    assert_eq!(first.status, 200);
    let cookie = first.headers.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap();
    let bare = SiteUrl { target: pool.target.split('?').next().unwrap().into(), ..pool.clone() };
    bare.post(body.clone()).await.code(403, "forbidden");
    let header = format!("Cookie: {cookie}\r\n");
    bare.raw("POST", Some(&bare.foreign_origin()), &header, &body.to_string()).await.code(403, "forbidden");
    no_command(&mut a).await;
    let origin = format!("http://{}", bare.host);
    bare.raw("POST", Some(&origin), &header, &body.to_string()).await.code(403, "forbidden");
    assert_eq!(pool.raw("POST", Some(&origin), &header, &body.to_string()).await.status, 202);
    command(&mut a, "goto_workspace").await;
}

#[tokio::test]
async fn read_states_lists_every_session_with_its_question_parts() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-states").await;
    let page = env.workspace_project_root.join("decision.html");
    let question = format!("Land it? Options: (a) merge; (b) send back. Recommend (a). Page: {}", page.display());
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST.to_uppercase(), "workspace_id": ROW, "repo": "demo",
        "state": "blocked", "summary": question, "question": question, "waiting": "build",
        "status_at": "status-time", "last_seen": "seen-time"}));
    set_entry(&env, "outside", json!({"host": TEST_STATE_HOST, "state": "waiting", "waiting": "job"}));
    set_entry(&env, "foreign", json!({"host": "other-daemon", "workspace_id": ROW, "state": "blocked"}));
    set_entry(&env, "unknown-host", json!({"state": "idle"}));
    let site = queue(&env, &mut a).await;
    let res = site.post(json!({"verb": "states"})).await;
    assert_eq!(res.status, 200);
    let body = res.json();
    assert_eq!(body["ok"], true);
    let rows = body["rows"].as_array().unwrap();
    assert_eq!(rows.iter().map(|r| r["handle"].as_str().unwrap()).collect::<Vec<_>>(), vec!["outside", HANDLE]);
    let row = &rows[1];
    assert_eq!(row["row"], ROW);
    assert_eq!(row["question"], "Land it?");
    assert_eq!(row["options"], json!([{"key": "a", "text": "merge"}, {"key": "b", "text": "send back"}]));
    assert_eq!(row["recommend"], "a");
    assert_eq!(row["page"], page.to_str().unwrap());
    for (key, value) in [("repo", "demo"), ("state", "blocked"), ("waiting", "build"), ("status_at", "status-time"), ("last_seen", "seen-time")] {
        assert_eq!(row[key], value);
    }
    assert_eq!(row["summary"], question);
    for key in ["row", "repo", "summary", "question", "recommend", "page", "status_at", "last_seen"] {
        assert_eq!(rows[0][key], "", "absent {key}");
    }
    assert_eq!(rows[0]["options"], json!([]));
    // No cached answer: the next request reads the new question.
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "question": "Changed?"}));
    assert_eq!(site.post(json!({"verb": "states"})).await.json()["rows"][1]["question"], "Changed?");
}

#[tokio::test]
async fn open_opens_only_the_page_the_rows_question_names_inside_its_folder() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-open").await;
    let site = queue(&env, &mut a).await;
    let page = env.workspace_project_root.join("decision.html");
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "question": format!("Decide? Page: {}", page.display())}));
    let res = site.post(json!({"verb": "open", "handle": HANDLE, "path": "ignored.html"})).await;
    assert_eq!(res.status, 202);
    let args = command(&mut a, "docs").await;
    assert_eq!(args["workspace"], ROW);
    let mut b = window(&env, "other-open-window").await;
    let wrong_owner = call(&mut b, 11, op::DOCS_OPEN, json!({"path": args["path"]})).await;
    assert_eq!(wrong_owner.payload["code"], "page_gone", "{wrong_owner:?}");
    let opened = call(&mut a, 11, op::DOCS_OPEN, json!({"path": args["path"]})).await;
    let granted = SiteUrl::parse(opened.payload["url"].as_str().expect("granted page URL"));
    assert_eq!(granted.raw("GET", None, "", "").await.body.as_slice(), b"<p>Decision</p>");
    let outside = env.daemon_project_root.join("outside.html");
    std::fs::write(&outside, "outside").unwrap();
    let mut paths = vec![outside.clone(), "relative.html".into(), env.workspace_project_root.join("missing.html")];
    #[cfg(unix)]
    {
        let link = env.workspace_project_root.join("linked.html");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        paths.push(link);
    }
    #[cfg(windows)]
    {
        let link = env.workspace_project_root.join("linked.html");
        std::os::windows::fs::symlink_file(&outside, &link).expect("the test runner must allow a scratch symlink");
        paths.push(link);
    }
    for path in paths {
        set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "question": format!("Decide? Page: {}", path.display())}));
        site.post(json!({"verb": "open", "handle": HANDLE})).await.code(403, "page_outside");
    }
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "question": "No page"}));
    site.post(json!({"verb": "open", "handle": HANDLE})).await.code(409, "no_page");
    no_command(&mut a).await;
}

/// Red at the first landing by behavior: its later docs.open follows the replacement into another known row.
#[tokio::test]
async fn a_decision_page_replaced_after_the_verb_cannot_escape_its_row() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-swap").await;
    let site = queue(&env, &mut a).await;
    let page = env.workspace_project_root.join("decision.html");
    let outside = env.daemon_project_root.join("other-row.html");
    std::fs::write(&outside, "other row's private page").unwrap();
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "question": format!("Decide? Page: {}", page.display())}));
    assert_eq!(site.post(json!({"verb": "open", "handle": HANDLE})).await.status, 202);
    let args = command(&mut a, "docs").await;
    std::fs::remove_file(&page).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &page).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(&outside, &page).expect("the runner must allow a scratch symlink");
    let opened = call(&mut a, 12, op::DOCS_OPEN, json!({"path": args["path"]})).await;
    let granted = SiteUrl::parse(opened.payload["url"].as_str().expect("row-confined page URL"));
    let reply = granted.raw("GET", None, "", "").await;
    assert_eq!(reply.status, 403, "{}", String::from_utf8_lossy(&reply.body));
    assert!(!reply.body.windows(b"private page".len()).any(|w| w == b"private page"));
    let nonce = granted.target.split('/').nth(1).unwrap();
    for path in ["../other-row.html", ".git/config"] {
        let forbidden = SiteUrl { target: format!("/{nonce}/{path}"), ..granted.clone() };
        assert_eq!(forbidden.raw("GET", None, "", "").await.status, 403);
    }
}

/// Red at b9a079069 by behavior: a different directory at the same pathname serves the other row's bytes.
#[cfg(unix)]
#[tokio::test]
async fn a_decision_grant_refuses_a_row_folder_replaced_after_the_grant() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-root-swap").await;
    let site = queue(&env, &mut a).await;
    let page = env.workspace_project_root.join("decision.html");
    std::fs::write(env.daemon_project_root.join("decision.html"), "other row's private bytes").unwrap();
    set_entry(&env, HANDLE, json!({"host": TEST_STATE_HOST, "question": format!("Decide? Page: {}", page.display())}));
    assert_eq!(site.post(json!({"verb": "open", "handle": HANDLE})).await.status, 202);
    let args = command(&mut a, "docs").await;
    let opened = call(&mut a, 12, op::DOCS_OPEN, json!({"path": args["path"]})).await;
    let granted = SiteUrl::parse(opened.payload["url"].as_str().unwrap());
    assert_eq!(granted.raw("GET", None, "", "").await.body.as_slice(), b"<p>Decision</p>");
    std::fs::create_dir(env.workspace_project_root.join("assets")).unwrap();
    std::fs::write(env.workspace_project_root.join("assets/app.js"), "relative asset").unwrap();
    let prefix = granted.target.rsplit_once('/').unwrap().0;
    let asset = SiteUrl { target: format!("{prefix}/assets/app.js"), ..granted.clone() };
    assert_eq!(asset.raw("GET", None, "", "").await.body.as_slice(), b"relative asset");
    let original = env._tmp.path().join("original-row");
    std::fs::rename(&env.workspace_project_root, &original).unwrap();
    std::os::unix::fs::symlink(&env.daemon_project_root, &env.workspace_project_root).unwrap();
    let reply = granted.raw("GET", None, "", "").await;
    assert!(matches!(reply.status, 403 | 404), "the replacement link is refused: {}", String::from_utf8_lossy(&reply.body));
    // A plain directory replacement defeats canonical-path equality too, so this is the behavioral red control.
    std::fs::remove_file(&env.workspace_project_root).unwrap();
    std::fs::rename(&env.daemon_project_root, &env.workspace_project_root).unwrap();
    let reply = granted.raw("GET", None, "", "").await;
    assert_eq!(reply.status, 403, "a different folder at the same path is refused: {}", String::from_utf8_lossy(&reply.body));
    std::fs::rename(&env.workspace_project_root, &env.daemon_project_root).unwrap();
    std::fs::rename(&original, &env.workspace_project_root).unwrap();
    assert_eq!(granted.raw("GET", None, "", "").await.body.as_slice(), b"<p>Decision</p>");
}

/// Red at the first landing by behavior: a damaged registry answered 200 with an empty rows array.
#[tokio::test]
async fn a_registry_failure_is_visible_and_recovers_on_the_next_read() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-registry-error").await;
    let site = queue(&env, &mut a).await;
    for bytes in [b"not JSON".as_slice(), b"{\"agents\":null}", b""] {
        support::set_registry_fixture(&env.comm_root, support::RegistryFixture::Bytes(bytes));
        site.post(json!({"verb": "states"})).await.code(500, "handler_error");
    }
    // A directory is unreadable as registry bytes even when tests run as root.
    support::set_registry_fixture(&env.comm_root, support::RegistryFixture::Directory);
    site.post(json!({"verb": "states"})).await.code(500, "handler_error");
    support::set_registry_fixture(&env.comm_root, support::RegistryFixture::Missing);
    site.post(json!({"verb": "states"})).await.code(500, "handler_error");
    support::set_registry_fixture(&env.comm_root, support::RegistryFixture::Bytes(b"{\"agents\":{}}"));
    let reply = site.post(json!({"verb": "states"})).await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json()["rows"], json!([]));
}

/// Red at the first landing by behavior: the old document rides the new document's browser cookie.
#[tokio::test]
async fn a_reassigned_pool_port_refuses_the_old_document_with_the_new_cookie() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-pool-grant").await;
    let root = env.workspace_project_root.join("pool");
    std::fs::create_dir(&root).unwrap();
    let entry = root.join("index.html");
    std::fs::write(&entry, "<script src=\"/a.js\"></script>").unwrap();
    let old = open(&mut a, &entry).await;
    assert_eq!(old.raw("GET", None, "", "").await.status, 200, "A's document loads before A goes away");
    let mut held = Vec::new();
    for i in 0..3 {
        let mut window = window(&env, &format!("pool-held-{i}")).await;
        open(&mut window, &entry).await;
        held.push(window);
    }
    drop(a);
    // A successful states read needs no window; goto shows when the roster sees A away.
    support::poll_until(|| async {
        (old.post(json!({"verb": "goto", "handle": HANDLE})).await.status == 503).then_some(())
    }, BOUND, "pool window A away").await;
    let mut b = window(&env, "pool-new").await;
    let new = open(&mut b, &entry).await;
    assert_eq!(old.host, new.host, "B takes A's away port");
    let first = new.raw("GET", None, "", "").await;
    assert_eq!(first.status, 200);
    let cookie = first.headers.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).unwrap().split(';').next().unwrap();
    let headers = format!("Cookie: {cookie}\r\n");
    let origin = format!("http://{}", new.host);
    let body = json!({"verb": "goto", "handle": HANDLE}).to_string();
    let bare = SiteUrl { target: old.target.split('?').next().unwrap().into(), ..old.clone() };
    bare.raw("POST", Some(&origin), &headers, &body).await.code(403, "forbidden");
    old.raw("POST", Some(&origin), &headers, &body).await.code(403, "forbidden");
    no_command(&mut b).await;
    let accepted = new.raw("POST", Some(&origin), &headers, &body).await;
    assert_eq!(accepted.status, 202);
    assert!(!accepted.headers.contains("Set-Cookie:"), "a verb never replaces the browser's file cookie");
    command(&mut b, "goto_workspace").await;
}

#[tokio::test]
async fn a_page_keeps_working_after_it_opens_another() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-sites").await;
    let first = queue(&env, &mut a).await;
    let decision = env.workspace_project_root.join("decision.html");
    for _ in 0..7 { open(&mut a, &decision).await; }
    assert_eq!(first.raw("GET", None, "", "").await.status, 200);
    assert_eq!(first.post(json!({"verb": "states"})).await.status, 200);
    open(&mut a, &decision).await;
    assert_eq!(first.raw("GET", None, "", "").await.status, 404);
    assert_eq!(first.post(json!({"verb": "states"})).await.status, 404);
}

#[tokio::test]
async fn a_page_whose_window_is_away_is_answered_503() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-away").await;
    let site = queue(&env, &mut a).await;
    drop(a);
    let refused = support::poll_until(|| async {
        let res = site.post(json!({"verb": "goto", "handle": HANDLE})).await;
        (res.status == 503).then_some(res)
    }, BOUND, "window away").await;
    refused.code(503, "window_gone");
    assert_eq!(site.post(json!({"verb": "states"})).await.status, 200);
    let mut back = window(&env, "window-a").await;
    assert_eq!(site.post(json!({"verb": "goto", "handle": HANDLE})).await.status, 202);
    command(&mut back, "goto_workspace").await;
}

#[tokio::test]
async fn two_requests_ride_one_connection() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-keep").await;
    let site = queue(&env, &mut a).await;
    let mut stream = site.connect().await;
    let body = json!({"verb": "states"}).to_string();
    let request = format!("POST {} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\nContent-Length: {}\r\n\r\n{body}", site.target, site.host, site.host, body.len());
    // Pipeline a second request behind the first body to exercise bytes read past each head.
    let res = exchange(&mut stream, &format!("{request}{request}"), false).await;
    assert_eq!(res.status, 200);
    assert!(!res.headers.contains("Connection: close"));
    assert_eq!(exchange(&mut stream, "", false).await.status, 200);
    let get = format!("GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", site.target, site.host);
    assert_eq!(exchange(&mut stream, &get, false).await.status, 200);
    let mut byte = [0; 1];
    assert_eq!(tokio::time::timeout(BOUND, stream.read(&mut byte)).await.unwrap().unwrap(), 0);
}

#[tokio::test]
async fn page_verbs_work_through_the_page_proxy_with_the_windows_local_origin() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-proxy").await;
    let site = queue(&env, &mut a).await;
    let port: u16 = site.host.rsplit_once(':').unwrap().1.parse().unwrap();
    let mut proxy = support::handoff(&env.socket_path, &Frame::req(2, op::PROXY_CONNECT, json!({"port": port}))).await;
    let (res, _) = tokio::time::timeout(BOUND, codec::read_frame(&mut proxy)).await.unwrap().unwrap();
    assert_eq!(res.payload["ok"], true, "{res:?}");
    // The browser sees a port chosen by the window, not the daemon's listener port.
    let local = "127.0.0.1:54321";
    for verb in ["states", "goto"] {
        let body = json!({"verb": verb, "handle": HANDLE}).to_string();
        let request = format!("POST {} HTTP/1.1\r\nHost: {local}\r\nOrigin: http://{local}\r\nContent-Length: {}\r\n\r\n{body}", site.target, body.len());
        let reply = exchange(&mut proxy, &request, false).await;
        assert_eq!(reply.status, if verb == "states" { 200 } else { 202 });
        assert_eq!(reply.json()["ok"], true);
    }
    command(&mut a, "goto_workspace").await;
}

#[tokio::test]
async fn a_handle_that_binds_no_row_here_is_refused() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-not-row").await;
    let site = queue(&env, &mut a).await;
    set_entry(&env, "outside", json!({"host": TEST_STATE_HOST}));
    set_entry(&env, "foreign", json!({"host": "other-daemon", "workspace_id": ROW}));
    // Even a name declared on a local row may not borrow a foreign registry entry.
    set_entry(&env, HANDLE, json!({"host": "other-daemon", "workspace_id": ROW}));
    for verb in ["goto", "open", "reply"] {
        for handle in ["outside", "missing", "foreign", HANDLE] {
            let res = site.post(json!({"verb": verb, "handle": handle, "text": "approve"})).await;
            res.code(409, "not_a_row");
            if handle == "foreign" || handle == HANDLE {
                assert!(res.json()["error"].as_str().unwrap().contains("other-daemon"));
            }
        }
    }
    no_command(&mut a).await;
}

#[tokio::test]
async fn malformed_verbs_and_unframed_or_oversize_bodies_run_nothing() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-bodies").await;
    let site = queue(&env, &mut a).await;
    for body in ["[]", "null", "{", "{}", "{\"verb\":\"goto\"}", "{\"verb\":\"open\",\"handle\":1}", "{\"verb\":\"reply\",\"handle\":\"row-here\"}", "{\"verb\":\"reply\",\"handle\":\"row-here\",\"text\":1}", "{\"verb\":\"confirm\"}"] {
        site.raw("POST", Some(&format!("http://{}", site.host)), "", body).await.code(400, "bad_request");
    }
    for (framing, status) in [("", 411), ("Transfer-Encoding: chunked\r\n", 411), ("Content-Length: 4097\r\n", 413), ("Content-Length: nope\r\n", 411), ("Content-Length: 2\r\nContent-Length: 2\r\n", 411)] {
        let mut stream = site.connect().await;
        let req = format!("POST {} HTTP/1.1\r\nHost: {}\r\nOrigin: http://{}\r\n{framing}\r\n", site.target, site.host, site.host);
        assert_eq!(exchange(&mut stream, &req, false).await.status, status);
    }
    let body = format!("{{\"verb\":\"states\"}}{}", " ".repeat(4096 - 17));
    assert_eq!(body.len(), 4096);
    assert_eq!(site.raw("POST", Some(&format!("http://{}", site.host)), "", &body).await.status, 200);
    for method in ["OPTIONS", "PUT", "DELETE", "PATCH"] {
        assert_eq!(site.raw(method, None, "", "").await.status, 405);
    }
    assert_eq!(site.raw("HEAD", None, "", "").await.status, 200);
    no_command(&mut a).await;
}

/// Hostile text is refused as bad_text, and a valid line from an unscoped page is refused as no_reply_scope: no window
/// is offered either.
#[tokio::test]
async fn a_reply_with_hostile_text_is_refused_and_opens_no_bar() {
    let _serial = SERIAL.lock().await;
    let (env, mut old) = setup("page-reply").await;
    let site = queue(&env, &mut old).await;
    let mut other = window(&env, "window-b").await;
    let mut newest = window(&env, "window-a").await;
    for text in ["\u{1b}[201~", "a\nb", "", "   ", "a\rb", "\u{2066}"] {
        site.post(json!({"verb": "reply", "handle": HANDLE, "text": text})).await.code(400, "bad_text");
    }
    site.post(json!({"verb": "reply", "handle": HANDLE, "text": "a".repeat(513)})).await.code(400, "bad_text");
    tokio::join!(no_command(&mut old), no_command(&mut other), no_command(&mut newest));
    for text in ["approve", " /clear "] {
        site.post(json!({"verb": "reply", "handle": HANDLE, "text": text})).await.code(403, "no_reply_scope");
    }
    tokio::join!(no_command(&mut old), no_command(&mut other), no_command(&mut newest));
}

/// Red at the base: fe.command.send publishes the reserved command.
#[tokio::test]
async fn a_session_cannot_open_a_reply_bar() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-reserved").await;
    let mut b = window(&env, "window-b").await;
    let (mut sender, _) = support::connect_and_hello(&env.socket_path).await;
    for target in [Value::Null, json!("fe@test-window")] {
        let res = call(&mut sender, 90, op::FE_COMMAND_SEND, json!({
            "cmd": "page_reply", "target": target,
            "args": {"workspace": ROW, "handle": HANDLE, "line": "[sot-page] approve"}
        })).await;
        assert_eq!(res.payload["code"], "reserved", "{res:?}");
        assert!(res.payload["error"].as_str().unwrap().contains("page_reply"));
        tokio::join!(no_command(&mut a), no_command(&mut b));
    }
}


#[tokio::test]
async fn page_reply_rechecks_the_mark_the_handle_and_the_target_without_resuming_it() {
    let _serial = SERIAL.lock().await;
    let (env, mut a) = setup("page-delivery-refusals").await;
    let send = |workspace: &str, handle: &str, line: &str| json!({
        "workspace_id": workspace, "handle": handle, "line": line
    });
    for line in ["approve", "[sot-page] a\nb", "[sot-page]  approve"] {
        let res = call(&mut a, 101, "page.reply", send(ROW, HANDLE, line)).await;
        assert_eq!(res.payload["outcome"], "refused", "{res:?}");
        assert!(res.payload["detail"].as_str().unwrap().starts_with("reply"), "{res:?}");
    }
    let res = call(&mut a, 102, "page.reply", send("missing-row", HANDLE, "[sot-page] approve")).await;
    assert_eq!(res.payload["code"], "unknown_workspace", "{res:?}");
    let res = call(&mut a, 103, "page.reply", send(ROW, "stale-handle", "[sot-page] approve")).await;
    assert_eq!(res.payload["outcome"], "refused", "{res:?}");
    assert_eq!(res.payload["detail"], "handle_changed");
    let res = call(&mut a, 104, "page.reply", send(ROW, HANDLE, "[sot-page] approve")).await;
    assert_eq!(res.payload["outcome"], "refused", "{res:?}");
    assert_eq!(res.payload["detail"], "not_typeable", "an inert or Codex row cannot be typed into");
    // A stopped Claude row exercises the Ready check without starting any process.
    env.seed_capsule_toml("stopped-reply", "stopped-slug", &env.workspace_project_root, "claude");
    env.seed_capsule_toml("codex-reply", "codex-reply", &env.workspace_project_root, "codex");
    env.kill_daemon_bounded().await;
    env.spawn_sotd_with_env(&[("SOT_DOCS_PORT", "0"), ("SOT_VIDEO_PORT", "0")]);
    let mut back = window(&env, "window-a").await;
    let joined = call(&mut back, 105, op::AGENT_JOIN, json!({"workspace_id": "stopped-reply", "handle": "stopped-handle"})).await;
    assert_eq!(joined.payload["ok"], true);
    let res = call(&mut back, 106, "page.reply", send("stopped-reply", "stopped-handle", "[sot-page] approve")).await;
    assert_eq!(res.payload["outcome"], "not_ready", "{res:?}");
    let alias = call(&mut back, 107, "page.reply", send("stopped-slug", "stopped-handle", "[sot-page] approve")).await;
    assert_eq!(alias.payload["code"], "unknown_workspace");
    let joined = call(&mut back, 108, op::AGENT_JOIN, json!({"workspace_id": "codex-reply", "handle": "codex-handle"})).await;
    assert_eq!(joined.payload["ok"], true);
    let codex = call(&mut back, 109, "page.reply", send("codex-reply", "codex-handle", "[sot-page] approve")).await;
    assert_eq!(codex.payload["outcome"], "refused");
    assert_eq!(codex.payload["detail"], "not_typeable");
    let listed = call(&mut back, 110, op::WORKSPACE_LIST, json!({})).await;
    let row = listed.payload["workspaces"].as_array().expect("workspaces").iter()
        .find(|r| r["workspace_id"] == "stopped-reply").expect("stopped row");
    assert_ne!(row["phase"], "ready");
}
