use super::*;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Instant;

const PAGE: &str = "http://127.0.0.1:9/0123456789abcdef0123456789abcdef?secret=s3cr3t&id=1";

/// Std passes an inherited standard handle to a Windows child as an inheritable copy, whatever `harden_own_stdio`
/// did to the window's own: the opener's child must be handed null ones, or it holds the window's log files open
/// for as long as the browser runs. Read as text through the production view, from the signature through the
/// function's closing brace. Every contiguous `.stdin`, `.stdout` and `.stderr` in the function's text, a method
/// call (however the text between the name and its parenthesis is spaced or commented) or a field read, is the null
/// setting; a call spelled `Command::stdin(...)`, or with a space or comment between the dot and the name, is
/// outside this check.
#[test]
fn the_opener_hands_its_child_no_inherited_stdio() {
    let source = sot_log::test_scan::without_test_modules(include_str!("browser_open.rs"));
    let mut lines = source.lines().skip_while(|l| !l.starts_with("pub fn spawn_opener("));
    let mut body = String::new();
    for line in lines.by_ref() {
        body.push_str(line);
        body.push('\n');
        if line == "}" {
            break;
        }
    }
    assert!(!body.is_empty(), "spawn_opener was not found");
    assert_eq!(body.matches(".spawn()").count(), 3, "spawn_opener no longer has its three starts");
    for handle in ["stdin", "stdout", "stderr"] {
        let setting = format!(".{handle}(std::process::Stdio::null())");
        assert_eq!(body.matches(&setting).count(), 3, "an opener's {handle} is not null");
        assert_eq!(body.matches(&format!(".{handle}")).count(), 3, "an opener's {handle} has a setter other than the null one");
    }
}

/// Open `PAGE`; return the address the opener was handed.
fn handed(ttl: Duration) -> String {
    let mut got = String::new();
    open_page_with(PAGE, ttl, |a| {
        got = a.to_string();
        Ok(())
    })
    .unwrap();
    got
}

fn host_port(addr: &str) -> String {
    addr.trim_start_matches("http://").trim_end_matches('/').to_string()
}

fn get(addr: &str, request: &[u8]) -> String {
    let run = || -> std::io::Result<String> {
        let mut s = TcpStream::connect(host_port(addr))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(request)?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out)
    };
    run().unwrap_or_default()
}

const GET: &[u8] = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n";

fn refused_within(addr: &str, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if TcpStream::connect(host_port(addr)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// ADR 0049, User isolation: whichever page is opened (the shapes of a `wglshow` figure, Pluto, a docs site and a
/// video), the opener's command line carries only the redirect's own address, never the page's path or secret.
#[test]
fn the_browser_is_handed_no_secret() {
    for page in [
        PAGE,
        "http://127.0.0.1:41234/0123456789abcdef0123456789abcdef",
        "http://127.0.0.1:1234/edit?id=1&secret=Ab12Cd34Ef56",
        "http://127.0.0.1:1237/__open/0123456789abcdef0123456789abcdef/",
        "http://127.0.0.1:1235/0123456789abcdef0123456789abcdef.mp4",
    ] {
        let mut got = String::new();
        open_page_with(page, Duration::from_secs(5), |a| {
            got = a.to_string();
            Ok(())
        })
        .unwrap();
        let port = got.strip_prefix("http://127.0.0.1:").and_then(|r| r.strip_suffix('/')).unwrap_or("");
        assert!(!port.is_empty() && port.chars().all(|c| c.is_ascii_digit()), "{got}");
        for fragment in ["0123456789abcdef", "secret", "Ab12Cd34", "/edit", "/__open", ".mp4"] {
            assert!(!got.contains(fragment), "{got} names {fragment} of {page}");
        }
    }
}

#[test]
fn the_first_get_from_this_account_is_redirected_then_the_redirect_closes() {
    let a = handed(Duration::from_secs(5));
    let reply = get(&a, GET);
    assert!(reply.starts_with("HTTP/1.1 302 "), "{reply}");
    assert!(reply.contains(&format!("\r\nLocation: {PAGE}\r\n")), "{reply}");
    assert!(reply.contains("\r\nReferrer-Policy: no-referrer\r\n"), "{reply}");
    assert!(reply.contains("\r\nCache-Control: no-store\r\n"), "{reply}");
    assert!(refused_within(&a, Duration::from_secs(2)), "the spent redirect still listens");
}

#[test]
fn an_unclaimed_redirect_closes_after_its_ttl() {
    let a = handed(Duration::from_millis(500));
    assert!(refused_within(&a, Duration::from_secs(5)), "the expired redirect still listens");
}

#[test]
fn only_a_get_spends_the_redirect() {
    let a = handed(Duration::from_secs(5));
    assert_eq!(get(&a, b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n"), "");
    assert!(get(&a, GET).starts_with("HTTP/1.1 302 "));
}

#[test]
fn an_address_that_could_forge_a_header_or_a_scheme_is_refused() {
    for bad in ["http://127.0.0.1:9/x\r\nSet-Cookie: a=b", "file:///etc/passwd", "javascript:alert(1)"] {
        let err = open_page_with(bad, Duration::from_secs(1), |_| panic!("opener called for {bad:?}"))
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{bad:?}");
    }
}

#[test]
fn origin_of_shows_no_path_query_or_userinfo() {
    assert_eq!(origin_of("http://127.0.0.1:41234/0123abcd"), "http://127.0.0.1:41234");
    assert_eq!(origin_of("http://127.0.0.1:1234/edit?secret=abc&id=1"), "http://127.0.0.1:1234");
    assert_eq!(origin_of("http://127.0.0.1:1234?secret=x"), "http://127.0.0.1:1234");
    assert_eq!(origin_of("https://u:p@example.org/x"), "https://example.org");
    assert_eq!(origin_of("javascript:alert(1)"), "javascript");
}

#[test]
fn a_browser_frame_opens_only_where_it_is_aimed() {
    assert!(opens_here(true, None, "fe@a"));
    assert!(!opens_here(false, None, "fe@a"));
    assert!(opens_here(false, Some("fe@a"), "fe@a"));
    assert!(!opens_here(false, Some("fe@b"), "fe@a"));
}
