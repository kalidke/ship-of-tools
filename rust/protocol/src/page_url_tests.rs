use super::*;

#[test]
fn loopback_port_from_url_is_loopback_only() {
    assert_eq!(loopback_port_from_url("http://127.0.0.1:1241/"), Some(1241));
    assert_eq!(loopback_port_from_url("http://localhost:45817/app?x=1"), Some(45817));
    assert_eq!(loopback_port_from_url("https://127.0.0.1:9000"), Some(9000));
    // Never allowlist an external host, a portless URL, or garbage.
    assert_eq!(loopback_port_from_url("http://10.0.0.5:1241/"), None);
    assert_eq!(loopback_port_from_url("http://example.com:80/"), None);
    assert_eq!(loopback_port_from_url("http://127.0.0.1/"), None);
    assert_eq!(loopback_port_from_url("file:///tmp/x"), None);
    // The window's own cases.
    assert_eq!(
        loopback_port_from_url("http://127.0.0.1:1237/foo/bar?secret=abc"),
        Some(1237)
    );
    assert_eq!(loopback_port_from_url("https://localhost:1235/tok"), Some(1235));
    assert_eq!(loopback_port_from_url("http://10.0.0.5:1234/"), None);
    assert_eq!(loopback_port_from_url("127.0.0.1:1241"), None);
    assert_eq!(loopback_port_from_url("http://127.0.0.1:notaport/"), None);
    // Edge classes shared by every copy of the grammar.
    assert_eq!(loopback_port_from_url("http://127.0.0.1:65536/"), None);
    assert_eq!(loopback_port_from_url("http://[::1]:80/"), None);
    assert_eq!(loopback_port_from_url("HTTP://127.0.0.1:80/"), None);
    assert_eq!(loopback_port_from_url("http://user@127.0.0.1:80/"), None);
    assert_eq!(loopback_port_from_url("http://127.0.0.1:80#x"), Some(80));
    assert_eq!(loopback_port_from_url("http://127.0.0.1:65535/"), Some(65535));
    assert_eq!(loopback_port_from_url("http://127.0.0.1:0080/"), Some(80));
    assert_eq!(loopback_port_from_url("http://127.0.0.1:+80/"), Some(80));
    assert_eq!(loopback_port_from_url("http://127.0.0.1:-1/"), None);
    assert_eq!(loopback_port_from_url("http://127.0.0.1:/"), None);
    assert_eq!(loopback_port_from_url("http://127.0.0.1:80:90/"), None);
    assert_eq!(loopback_port_from_url("http://127.0.0.1:80?secret=t"), Some(80));
    assert_eq!(loopback_port_from_url("http://LOCALHOST:80/"), None);
    assert_eq!(loopback_port_from_url("https://localhost:80"), Some(80));
}

#[test]
fn with_loopback_port_replaces_only_the_port() {
    let cases = [
        ("http://127.0.0.1:1236/site/a.html?secret=s#top", Some("http://127.0.0.1:50001/site/a.html?secret=s#top")),
        ("https://localhost:1235/v?token=t", Some("https://localhost:50001/v?token=t")),
        ("http://127.0.0.1:1234", Some("http://127.0.0.1:50001")),
        ("http://127.0.0.1:80?secret=t", Some("http://127.0.0.1:50001?secret=t")),
        ("http://127.0.0.1:0080/", Some("http://127.0.0.1:50001/")),
        ("http://192.0.2.5:1236/", None),
        ("http://127.0.0.1/", None),
        ("http://user@127.0.0.1:80/", None),
        ("file:///tmp/x", None),
    ];
    for (url, want) in cases {
        assert_eq!(with_loopback_port(url, 50001).as_deref(), want, "{url}");
    }
}
