//! Tests of the prefix and pool listeners: pool assignment, bind fallback, serving a prefix site.

use super::*;

#[cfg(test)]
mod pool_tests {
    use super::*;

    fn reset() {
        POOL.write().unwrap_or_else(|p| p.into_inner()).clear();
    }

    #[test]
    fn assign_reuse_exhaust_free() {
        reset();
        let base = site_port();
        // Serials far outside anything other tests produce: clients.rs tests
        // drop ClientGuards with small serials, and each drop calls
        // remove_root(serial) — with colliding serials a parallel test run
        // freed this test's pool entries mid-assert.
        const S: u64 = 9_000_000_001;
        // Four distinct connections fill the pool in order.
        let (p1, s1) = assign_pool_port(S + 1, Site::plain(PathBuf::from("/a"))).unwrap();
        let (p2, s2) = assign_pool_port(S + 2, Site::plain(PathBuf::from("/b"))).unwrap();
        let (p3, _s3) = assign_pool_port(S + 3, Site::plain(PathBuf::from("/c"))).unwrap();
        let (p4, _s4) = assign_pool_port(S + 4, Site::plain(PathBuf::from("/d"))).unwrap();
        assert_eq!(
            vec![p1, p2, p3, p4],
            (base + 1..=base + POOL_SIZE).collect::<Vec<_>>()
        );
        // Distinct sites get distinct secrets.
        assert_ne!(s1, s2);
        assert!(s1.len() == 32 && s1.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        // A re-open by an existing owner REPOINTS its port, not a new one,
        // and mints a FRESH secret — the old one (and any cookie it set)
        // stops working (security review).
        let (p2_again, s2_again) = assign_pool_port(S + 2, Site::plain(PathBuf::from("/b2"))).unwrap();
        assert_eq!(p2_again, p2);
        assert_ne!(s2_again, s2);
        assert!(s2_again.len() == 32 && s2_again.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        assert_eq!(
            pool_entry_for(p2).map(|(r, _)| r.content_root.clone()),
            Some(PathBuf::from("/b2"))
        );
        // Fifth connection: exhausted.
        assert_eq!(assign_pool_port(S + 5, Site::plain(PathBuf::from("/e"))), None);
        assert_eq!(pool_in_use(), 4);
        // Disconnect frees exactly the owner's port; next claim gets it.
        remove_root(S + 3);
        assert_eq!(pool_in_use(), 3);
        let (p5, _s5) = assign_pool_port(S + 5, Site::plain(PathBuf::from("/e"))).unwrap();
        assert_eq!(p5, p3);
        reset();
    }

    /// PAGE-PORT: one browser can hold two sites at one pool port number (two daemons' pools through a remote window's
    /// page proxy), and cookies are not separated by port, so each open's cookie has a name of its own and a second
    /// site's first response never overwrites the first's cookie.
    #[test]
    fn two_opens_of_one_pool_port_number_name_their_cookies_apart() {
        let (a, b) = ("0123456789abcdef0123456789abcdef", "fedcba9876543210fedcba9876543210");
        assert_ne!(pool_cookie_name(a), pool_cookie_name(b));
        assert_eq!(pool_cookie_name(a), "sot_pool_0123456789abcdef");
    }
}

#[cfg(test)]
mod nonce_tests {
    use super::*;

    #[test]
    fn set_root_mints_a_lowercase_hex_nonce() {
        let serial = 8_000_000_101;
        let nonce = set_root(serial, Site::plain(PathBuf::from("/mw09"))).expect("nonce");
        assert!(nonce.len() == 32 && nonce.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f')));
        remove_root(serial);
    }
}

#[cfg(test)]
mod bind_fallback_tests {
    use super::*;

    /// Preferred port taken → `spawn` retries, then falls back to an
    /// OS-assigned port and records it for `bound_site_port()`.
    /// `start_paused` collapses the 10×300ms retry loop's sleeps.
    #[tokio::test(start_paused = true)]
    async fn spawn_falls_back_to_ephemeral_when_preferred_taken() {
        let squatter = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let taken = squatter.local_addr().unwrap().port();
        spawn(taken).await.expect("fallback bind should succeed");
        let bound = bound_site_port().expect("actual port recorded");
        assert_ne!(bound, taken, "must not claim the squatted port");
    }
}

#[cfg(test)]
mod prefix_serve_tests {
    use super::*;

    /// Write `contents` at `root/rel`, creating parent directories as needed.
    fn write_asset(root: &Path, rel: &str, contents: &[u8]) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(full, contents).unwrap();
    }

    /// Issue a raw `GET path` against the prefix server at `addr` and return
    /// `(status_code, content_type, body)`. Minimal hand-rolled HTTP/1.1
    /// client — mirrors the server's own hand-rolled parsing, no crate needed
    /// for a handful of headers over loopback.
    ///
    /// `read_to_end` is bounded by a timeout (codex review): every response
    /// here closes the connection (`Connection: close`), so a healthy server
    /// always hits EOF quickly — a server-side regression that stops writing
    /// or stops closing should fail this test fast, not hang the CI job
    /// until its overall timeout.
    async fn get(addr: std::net::SocketAddr, path: &str) -> (u16, Option<String>, Vec<u8>) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut raw = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_to_end(&mut raw),
        )
        .await
        .expect("server did not close the connection within 5s")
        .unwrap();
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("response must have a header/body separator");
        let head = String::from_utf8_lossy(&raw[..split]);
        let body = raw[split + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or("");
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let ctype = lines.find_map(|l| {
            l.split_once(':').and_then(|(k, v)| {
                k.trim()
                    .eq_ignore_ascii_case("content-type")
                    .then(|| v.trim().to_string())
            })
        });
        (status, ctype, body)
    }

    /// Field-report regression: `docs.open`/`o` need the WHOLE site reachable
    /// under one root, not just the entry file — a synthetic site with a
    /// stylesheet, a script, an image, and a nested page all linked
    /// page-relatively, all served with the right content-type, plus the
    /// traversal guard refusing an escape past the root.
    #[tokio::test]
    async fn serves_index_and_relative_subresources_refuses_traversal() {
        // Unique by construction, not by clock: this name carried the clock
        // ALONE — not even a pid — so it was not separated across processes
        // either. Same premise as the race that reddened a macOS leg.
        let base = std::env::temp_dir()
            .join(format!("sot-site-serve-test-{}", sot_updater::unique::suffix()));
        let root = base.join("site");
        std::fs::create_dir_all(&root).unwrap();
        write_asset(
            &root,
            "index.html",
            b"<html><head><link rel=stylesheet href=assets/site.css>\
              <script src=assets/site.js></script></head>\
              <body><img src=figures/x.png><a href=pages/two.html>two</a></body></html>",
        );
        write_asset(&root, "assets/site.css", b"body{}");
        write_asset(&root, "assets/site.js", b"console.log(1)");
        write_asset(&root, "figures/x.png", &[0x89, b'P', b'N', b'G']);
        write_asset(&root, "pages/two.html", b"<html>two</html>");
        // Outside the root — a canonicalizing traversal target that EXISTS
        // (so the guard is proven by the root-containment check, not just
        // by the file happening not to exist).
        std::fs::write(base.join("secret.txt"), b"nope").unwrap();

        // Serial far outside anything other tests in this file produce
        // (pool_tests uses 9_000_000_00x) — parallel test runs share the
        // same static maps.
        const SERIAL: u64 = 8_000_000_001;
        let nonce = set_root(SERIAL, Site::plain(root.clone())).expect("set_root");

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(async move {
                            let _ = handle_conn(stream, ServeMode::Prefix).await;
                        });
                    }
                    Err(_) => break,
                }
            }
        });

        let (status, ctype, body) = get(addr, &format!("/{nonce}/")).await;
        assert_eq!(status, 200, "index via directory-index");
        assert_eq!(ctype.as_deref(), Some("text/html; charset=utf-8"));
        assert!(!body.is_empty());

        for (rel, expected_ctype, expected_body) in [
            (
                "assets/site.css",
                "text/css; charset=utf-8",
                Some(&b"body{}"[..]),
            ),
            (
                "assets/site.js",
                "text/javascript; charset=utf-8",
                Some(&b"console.log(1)"[..]),
            ),
            ("figures/x.png", "image/png", None),
            (
                "pages/two.html",
                "text/html; charset=utf-8",
                Some(&b"<html>two</html>"[..]),
            ),
        ] {
            let (status, ctype, body) = get(addr, &format!("/{nonce}/{rel}")).await;
            assert_eq!(status, 200, "GET {rel} should 200");
            assert_eq!(
                ctype.as_deref(),
                Some(expected_ctype),
                "GET {rel} content-type"
            );
            if let Some(expected) = expected_body {
                assert_eq!(body, expected, "GET {rel} body");
            }
        }

        // A `../` escape past the root is refused even though the target
        // exists on disk.
        let (status, _, _) = get(addr, &format!("/{nonce}/../secret.txt")).await;
        assert_eq!(status, 403, "traversal escape must be refused");

        remove_root(SERIAL);
        let _ = std::fs::remove_dir_all(&base);
    }
}
