use super::*;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use serde_json::json;
use verbs::RowRef;

/// Admission happens before every capability closure, including the read-only states verb.
#[tokio::test]
async fn a_refused_page_request_calls_no_capability_and_sends_no_command() {
    let root = tempfile::tempdir().unwrap();
    const OWNER: u64 = 8_000_000_051;
    let nonce = set_root(OWNER, Site::plain(root.path().to_path_buf())).unwrap();
    let (reads, lookups, windows) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (fe, mut commands) = tokio::sync::broadcast::channel(16);
    let (r, l, w) = (reads.clone(), lookups.clone(), windows.clone());
    let verbs = Arc::new(PageVerbs::new(
        Arc::new(move || { r.fetch_add(1, SeqCst); Ok(Vec::new()) }),
        Arc::new(move |_| {
            l.fetch_add(1, SeqCst);
            Ok(RowRef { workspace_id: "row".into(), project_root: PathBuf::new(), page: String::new() })
        }),
        Arc::new(move |owner| {
            assert_eq!(owner, OWNER);
            w.fetch_add(1, SeqCst);
            Some(crate::clients::ActiveFrontend { serial: 42, handle: "fe@test".into() })
        }),
        fe,
    ));
    let listener = crate::pages::bind_page_listener(0).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(sot_platform::identity::peer_owner::serve_own(listener, "page-test", move |stream| {
        let verbs = verbs.clone();
        async move { handle_conn(stream, ServeMode::Prefix, verbs).await.expect("page connection"); }
    }));
    let send = |path: String, headers: String, body: String| async move {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(format!("POST {path} HTTP/1.1\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes)).await.unwrap().unwrap();
        let text = String::from_utf8(bytes).unwrap();
        text.split_whitespace().nth(1).unwrap().parse::<u16>().unwrap()
    };
    let own = format!("Host: {addr}\r\nOrigin: http://{addr}\r\n");
    let states = json!({"verb": "states"}).to_string();
    assert_eq!(send(format!("/{nonce}/no-file-needed"), own.clone(), states.clone()).await, 200);
    assert_eq!(reads.load(SeqCst), 1);
    for verb in ["states", "goto"] {
        let body = json!({"verb": verb, "handle": "handle"}).to_string();
        assert_eq!(send("/unknown/".into(), own.clone(), body.clone()).await, 404);
        for headers in [
            format!("Host: {addr}\r\n"),
            format!("Host: {addr}\r\nOrigin: null\r\n"),
            format!("Host: {addr}\r\nOrigin: http://127.0.0.1:{}\r\n", if addr.port() == 1 { 2 } else { 1 }),
            format!("Origin: http://{addr}\r\n"),
            format!("{own}Origin: http://{addr}\r\n"),
            format!("{own}Host: {addr}\r\n"),
        ] {
            assert_eq!(send(format!("/{nonce}/"), headers, body.clone()).await, 403);
        }
    }
    assert_eq!(reads.load(SeqCst), 1);
    assert_eq!(lookups.load(SeqCst), 0);
    assert_eq!(windows.load(SeqCst), 0);
    assert!(commands.try_recv().is_err());
    let body = json!({"verb": "goto", "handle": "handle"}).to_string();
    assert_eq!(send(format!("/{nonce}/"), own, body).await, 202);
    let command = commands.try_recv().expect("the admitted command");
    assert_eq!(command.target_serial, Some(42));
    assert_eq!(command.args, json!({"workspace": "row"}));
    assert_eq!(lookups.load(SeqCst), 1);
    assert_eq!(windows.load(SeqCst), 1);
    remove_root(OWNER);
}

#[tokio::test]
async fn a_failed_page_read_is_a_json_error_and_the_connection_stays() {
    let root = tempfile::tempdir().unwrap();
    const OWNER: u64 = 8_000_000_052;
    let nonce = set_root(OWNER, Site::plain(root.path().to_path_buf())).unwrap();
    let (fe, mut commands) = tokio::sync::broadcast::channel(16);
    let calls = AtomicUsize::new(0);
    let verbs = Arc::new(PageVerbs::new(
        Arc::new(move || {
            if calls.fetch_add(1, SeqCst) == 0 { panic!("fixture read failed"); }
            Ok(Vec::new())
        }),
        Arc::new(|_| panic!("read must not look up a handle")),
        Arc::new(|_| panic!("read needs no window")),
        fe,
    ));
    let listener = crate::pages::bind_page_listener(0).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_conn(stream, ServeMode::Prefix, verbs).await.unwrap();
    });
    let mut stream = TcpStream::connect(addr).await.unwrap();
    for status in [500, 200] {
        let body = json!({"verb": "states"}).to_string();
        stream.write_all(format!("POST /{nonce}/ HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        let read = async {
            let mut head = Vec::new();
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") {
                assert_eq!(stream.read(&mut byte).await.unwrap(), 1);
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            assert_eq!(head.split_whitespace().nth(1).unwrap().parse::<u16>().unwrap(), status);
            assert!(head.contains("Cache-Control: no-store\r\n"));
            let len: usize = head.lines().find_map(|line| line.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
            let mut body = vec![0; len];
            stream.read_exact(&mut body).await.unwrap();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        };
        let answer = tokio::time::timeout(Duration::from_secs(5), read).await.unwrap();
        if status == 500 { assert_eq!(answer["code"], "handler_error"); }
        else { assert_eq!(answer["ok"], true); }
    }
    assert!(commands.try_recv().is_err());
    remove_root(OWNER);
}

#[tokio::test]
async fn an_expired_grant_cannot_finish_a_verb_that_was_already_reading() {
    let (entered, mut started) = tokio::sync::mpsc::channel(1);
    let (release, wait) = std::sync::mpsc::channel();
    let wait = std::sync::Mutex::new(wait);
    let (fe, mut commands) = tokio::sync::broadcast::channel(16);
    let verbs = Arc::new(PageVerbs::new(
        Arc::new(move || {
            entered.try_send(()).unwrap();
            wait.lock().unwrap().recv().unwrap();
            Ok(Vec::new())
        }),
        Arc::new(|_| panic!("states must not look up a handle")),
        Arc::new(|_| panic!("states needs no window")), fe,
    ));
    const OWNER: u64 = 8_000_000_054;
    let root = tempfile::tempdir().unwrap();
    let nonce = set_root(OWNER, Site::plain(root.path().to_path_buf())).unwrap();
    let listener = crate::pages::bind_page_listener(0).unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_conn(stream, ServeMode::Prefix, verbs).await.unwrap();
    });
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body = "{\"verb\":\"states\"}";
    stream.write_all(format!("POST /{nonce}/ HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), started.recv()).await.unwrap().unwrap();
    remove_root(OWNER);
    release.send(()).unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut bytes)).await.unwrap().unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    assert_eq!(head.split_whitespace().nth(1), Some("403"));
    assert_eq!(serde_json::from_str::<serde_json::Value>(body).unwrap()["code"], "forbidden");
    assert!(commands.try_recv().is_err());
    server.await.unwrap();
}

struct ReleaseWorkers(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl Drop for ReleaseWorkers {
    fn drop(&mut self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

async fn budget_post(addr: std::net::SocketAddr, nonce: &str, verb: &str) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body = json!({"verb": verb, "handle": "handle"}).to_string();
    stream.write_all(format!("POST /{nonce}/ HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await.unwrap();
    let text = String::from_utf8(bytes).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    (head.split_whitespace().nth(1).unwrap().parse::<u16>().unwrap(), serde_json::from_str(body).unwrap())
}

async fn budget_file(addr: std::net::SocketAddr, nonce: &str) {
    let mut file = TcpStream::connect(addr).await.unwrap();
    file.write_all(format!("GET /{nonce}/index.html HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), file.read_to_end(&mut bytes)).await.expect("files remain available").unwrap();
    let reply = String::from_utf8(bytes).unwrap();
    assert_eq!(reply.split_whitespace().nth(1), Some("200"));
    assert!(reply.ends_with("files still serve"));
}

/// Held actions retain their slots after timeout, while states uses its own budget.
#[tokio::test]
async fn page_work_is_limited_and_a_timeout_keeps_its_slot_until_the_worker_ends() {
    budget_case(false, 8_000_000_053).await;
}

#[tokio::test]
async fn polling_has_its_own_bounded_budget_and_does_not_block_actions() {
    budget_case(true, 8_000_000_055).await;
}

#[tokio::test]
async fn incomplete_bodies_have_a_bounded_budget_that_releases_on_eof() {
    const OWNER: u64 = 8_000_000_056;
    let root = tempfile::tempdir().unwrap();
    let nonce = set_root(OWNER, Site::plain(root.path().to_path_buf())).unwrap();
    let verbs = PageVerbs::empty();
    let listener = crate::pages::bind_page_listener(0).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(sot_platform::identity::peer_owner::serve_own(listener, "page-body-limit-test", move |stream| {
        let verbs = verbs.clone();
        async move { handle_conn(stream, ServeMode::Prefix, verbs).await.unwrap(); }
    }));
    assert_eq!(budget_post(addr, &nonce, "states").await.0, 200);
    let mut readers = Vec::new();
    for _ in 0..4 {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(format!("POST /{nonce}/ HTTP/1.1\r\nHost: {addr}\r\nOrigin: http://{addr}\r\nContent-Length: 17\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        readers.push(stream);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, body) = budget_post(addr, &nonce, "states").await;
            if status == 503 { assert_eq!(body["code"], "server_busy"); break; }
            assert_eq!(status, 200, "{body}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("all four body readers admitted, the fifth refused");
    for mut stream in readers {
        stream.shutdown().await.unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), stream.read_to_end(&mut reply)).await.unwrap().unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert_eq!(reply.split_whitespace().nth(1), Some("400"));
    }
    assert_eq!(budget_post(addr, &nonce, "states").await.0, 200, "EOF released the body readers");
    remove_root(OWNER);
}

async fn budget_case(states_stalled: bool, owner: u64) {
    use std::sync::{Condvar, Mutex};
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let release = ReleaseWorkers(gate.clone());
    let started = Arc::new(AtomicUsize::new(0));
    let ended = Arc::new(AtomicUsize::new(0));
    let (s, e) = (started.clone(), ended.clone());
    let work = Arc::new(move || {
        s.fetch_add(1, SeqCst);
        let mut released = gate.0.lock().unwrap();
        while !*released { released = gate.1.wait(released).unwrap(); }
        e.fetch_add(1, SeqCst);
    });
    let states_work = work.clone();
    let windows = Arc::new(AtomicUsize::new(0));
    let looked_up = windows.clone();
    let (fe, mut commands) = tokio::sync::broadcast::channel(16);
    let verbs = Arc::new(PageVerbs::new(
        Arc::new(move || {
            if states_stalled { states_work(); }
            Ok(Vec::new())
        }),
        Arc::new(move |_| {
            if !states_stalled { work(); }
            Ok(RowRef { workspace_id: "row".into(), project_root: PathBuf::new(), page: String::new() })
        }),
        Arc::new(move |requested| {
            assert_eq!(requested, owner);
            looked_up.fetch_add(1, SeqCst);
            Some(crate::clients::ActiveFrontend { serial: 42, handle: "fe@test".into() })
        }), fe,
    ));
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("index.html"), "files still serve").unwrap();
    let nonce = set_root(owner, Site::plain(root.path().to_path_buf())).unwrap();
    let listener = crate::pages::bind_page_listener(0).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(sot_platform::identity::peer_owner::serve_own(listener, "page-limit-test", move |stream| {
        let verbs = verbs.clone();
        async move { handle_conn(stream, ServeMode::Prefix, verbs).await.unwrap(); }
    }));
    let post = |nonce: String, verb: &'static str| async move { budget_post(addr, &nonce, verb).await };
    let (stalled, healthy, success) = if states_stalled { ("states", "goto", 202) } else { ("goto", "states", 200) };
    let requests: Vec<_> = (0..4).map(|_| tokio::spawn(post(nonce.clone(), stalled))).collect();
    tokio::time::timeout(Duration::from_secs(3), async {
        while started.load(SeqCst) != 4 { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.expect("four workers admitted");
    let (status, body) = tokio::time::timeout(Duration::from_secs(1), post(nonce.clone(), stalled)).await.expect("excess work refused immediately");
    assert_eq!(status, 503);
    assert_eq!(body["code"], "server_busy");
    let (status, body) = tokio::time::timeout(Duration::from_secs(1), post(nonce.clone(), healthy)).await.expect("the other budget makes progress");
    assert_eq!(status, success, "{body}");
    if states_stalled { commands.try_recv().expect("the healthy goto"); }
    else { assert_eq!(body["rows"], json!([])); }
    budget_file(addr, &nonce).await;
    for request in requests {
        let (status, body) = tokio::time::timeout(Duration::from_secs(7), request).await.expect("handler deadline").unwrap();
        assert_eq!(status, 503);
        assert_eq!(body["code"], "handler_timeout");
    }
    let (status, body) = tokio::time::timeout(Duration::from_secs(1), post(nonce.clone(), stalled)).await.unwrap();
    assert_eq!(status, 503);
    assert_eq!(body["code"], "server_busy", "timed-out blocking work still owns its slots");
    let (status, body) = tokio::time::timeout(Duration::from_secs(1), post(nonce.clone(), healthy)).await.expect("polling and actions stay independent after timeout");
    assert_eq!(status, success, "{body}");
    if states_stalled { commands.try_recv().expect("the second healthy goto"); }
    assert_eq!(started.load(SeqCst), 4);
    drop(release);
    tokio::time::timeout(Duration::from_secs(3), async {
        while ended.load(SeqCst) != 4 { tokio::time::sleep(Duration::from_millis(10)).await; }
    }).await.unwrap();
    assert!(commands.try_recv().is_err(), "timed-out actions send no late command");
    assert_eq!(windows.load(SeqCst), if states_stalled { 2 } else { 0 });
    // The workers may still be dropping their permits after incrementing ended.
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (status, body) = post(nonce.clone(), stalled).await;
            if status != 503 {
                assert_eq!(status, if states_stalled { 200 } else { 202 }, "{body}");
                break;
            }
            assert_eq!(body["code"], "server_busy");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("finished workers release their slots");
    if !states_stalled { assert_eq!(commands.try_recv().unwrap().target_serial, Some(42)); }
    remove_root(owner);
}
