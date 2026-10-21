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
#[path = "page_url_tests.rs"]
mod tests;
