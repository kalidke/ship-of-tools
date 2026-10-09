//! The loopback page-URL grammar: `http(s)://` and a host that is exactly `127.0.0.1` or `localhost`, then `:<port>`
//! (the authority ends at the first `/`, `?` or `#`; the port is what `u16` parsing accepts after its last `:`, so `:0`
//! and `:+80` give a port) yields that port, anything else `None`.
//! Owner: pages; here because the daemon and the window both link this crate.

/// Parse the port out of a loopback `http(s)://` URL — `None` for any
/// non-loopback host (never allowlist an external address), so only the
/// daemon's own loopback pages arm a proxy listener.
pub fn loopback_port_from_url(url: &str) -> Option<u16> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (host, port) = authority.rsplit_once(':')?;
    if host != "127.0.0.1" && host != "localhost" {
        return None;
    }
    port.parse().ok()
}

/// The same loopback URL with its port replaced by `port`; scheme, host, path, query and fragment are kept, and any URL
/// `loopback_port_from_url` refuses is `None`. A remote daemon's page port is a port on the daemon's computer: the
/// window opens the page at its own listener's port instead (ADR 0035, the window's half).
pub fn with_loopback_port(url: &str, port: u16) -> Option<String> {
    loopback_port_from_url(url)?;
    let (scheme, rest) = url.split_at(url.find("://")? + 3);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (host, _) = rest[..end].rsplit_once(':')?;
    Some(format!("{scheme}{host}:{port}{}", &rest[end..]))
}

#[cfg(test)]
mod tests {
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
}
