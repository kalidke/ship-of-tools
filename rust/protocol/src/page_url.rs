//! The loopback page-URL grammar: `http(s)://127.0.0.1|localhost:<port>/…` yields its port, anything else `None`.
//! Owner: pages; here because the daemon and the window both link this crate.

/// Parse the port out of a loopback `http(s)://` URL — `None` for any
/// non-loopback host (never allowlist an external address).
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
}
