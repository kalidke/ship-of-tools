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
    let verbs = Arc::new(PageVerbs {
        states: Arc::new(move || { r.fetch_add(1, SeqCst); Vec::new() }),
        row_of: Arc::new(move |_| {
            l.fetch_add(1, SeqCst);
            Ok(RowRef { workspace_id: "row".into(), project_root: PathBuf::new(), page: String::new() })
        }),
        window: Arc::new(move |owner| {
            assert_eq!(owner, OWNER);
            w.fetch_add(1, SeqCst);
            Some(crate::clients::ActiveFrontend { serial: 42, handle: "fe@test".into() })
        }),
        fe,
    });
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
    let verbs = Arc::new(PageVerbs {
        states: Arc::new(move || {
            if calls.fetch_add(1, SeqCst) == 0 { panic!("fixture read failed"); }
            Vec::new()
        }),
        row_of: Arc::new(|_| panic!("read must not look up a handle")),
        window: Arc::new(|_| panic!("read needs no window")),
        fe,
    });
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
