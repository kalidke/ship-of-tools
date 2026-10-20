//! The pool listener's auth: a request that carries a `secret` query parameter is decided by that parameter alone, and
//! the cookie decides only a request with none (#38, a stale entry URL met the port's new cookie).

use super::tests::own_pool;
use super::*;

/// One `GET` with `Connection: close` against the pool listener: the status, and the `Set-Cookie` value if any.
async fn get(port: u16, target: &str, cookie: Option<&str>) -> (u16, Option<String>) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let cookie = cookie.map(|c| format!("Cookie: {c}\r\n")).unwrap_or_default();
    s.write_all(format!("GET {target} HTTP/1.1\r\n{cookie}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(5), s.read_to_end(&mut raw))
        .await
        .expect("server did not close the connection within 5s")
        .unwrap();
    let head = String::from_utf8_lossy(&raw).into_owned();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    let set = head.lines().find_map(|l| l.strip_prefix("Set-Cookie: ")).map(|c| c.trim().to_string());
    (status, set)
}

/// One case per class of request. `old` is the secret of the entry URL the stale tab holds, `new` the port's current
/// one; the rule for a repeated parameter is the first `secret` decides (`secret=bad&secret=good` is refused). A key
/// is matched after percent-decoding and in any case, and a key with no `=` is present and empty, so each spelling of
/// `secret` decides alone.
#[tokio::test]
async fn a_secret_parameter_decides_alone_and_the_cookie_only_without_one() {
    let _pool = own_pool();
    let root = std::env::temp_dir().join(format!("sot-site-auth-test-{}", sot_updater::unique::suffix()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("a.txt"), b"one").unwrap();
    const OWNER: u64 = 8_000_000_031;
    let (port, old) = assign_pool_port(OWNER, Site::plain(root.clone()), Vec::new).unwrap();
    let (_, new) = assign_pool_port(OWNER, Site::plain(root.clone()), Vec::new).unwrap();
    let lport = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let lport = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = handle_conn(stream, ServeMode::Pool(port)).await;
                });
            }
        });
        lport
    };
    let name = pool_cookie_name(pool_cookie_host(), port);
    let fresh = format!("{name}={new}; HttpOnly; SameSite=Strict; Path=/");
    let stale = format!("{name}={old}");
    let valid = format!("{name}={new}");
    // (case, target, cookie header, status, Set-Cookie)
    let cases: Vec<(&str, String, Option<&str>, u16, Option<&str>)> = vec![
        ("valid query, no cookie", format!("/a.txt?secret={new}"), None, 200, Some(&fresh)),
        ("valid query, stale cookie", format!("/a.txt?secret={new}"), Some(&stale), 200, Some(&fresh)),
        ("stale query, valid cookie", format!("/a.txt?secret={old}"), Some(&valid), 403, None),
        ("no query, valid cookie", "/a.txt".into(), Some(&valid), 200, None),
        ("neither", "/a.txt".into(), None, 403, None),
        ("empty secret, valid cookie", "/a.txt?secret=".into(), Some(&valid), 403, None),
        ("bare secret, valid cookie", "/a.txt?secret".into(), Some(&valid), 403, None),
        ("bare secret, then a valid one", format!("/a.txt?secret&secret={new}"), None, 403, None),
        ("capitalized key, valid query, no cookie", format!("/a.txt?Secret={new}"), None, 200, Some(&fresh)),
        ("capitalized key, stale query, valid cookie", format!("/a.txt?Secret={old}"), Some(&valid), 403, None),
        ("capitalized key, empty, valid cookie", "/a.txt?Secret=".into(), Some(&valid), 403, None),
        ("encoded key, valid query, no cookie", format!("/a.txt?%73ecret={new}"), None, 200, Some(&fresh)),
        ("encoded key, stale query, valid cookie", format!("/a.txt?%73ecret={old}"), Some(&valid), 403, None),
        ("encoded key, empty, valid cookie", "/a.txt?%73ecret=".into(), Some(&valid), 403, None),
        ("bad then good", format!("/a.txt?secret={old}&secret={new}"), None, 403, None),
        ("good then bad", format!("/a.txt?secret={new}&secret={old}"), None, 200, Some(&fresh)),
    ];
    for (case, target, cookie, status, set) in cases {
        let got = get(lport, &target, cookie).await;
        assert_eq!(got, (status, set.map(str::to_string)), "{case}");
    }
    remove_root(OWNER);
    let _ = std::fs::remove_dir_all(&root);
}
