//! Opening a served page in the OS browser without its address on any command line (decision 0031). A page address
//! can carry a secret (`wglshow`'s path, Pluto's `?secret=`, a docs or video nonce), and on Linux every process's
//! arguments are readable by other accounts (`/proc/<pid>/cmdline`), so neither the opener nor the browser it starts
//! may be handed one. [`open_page`] binds a loopback listener of its own on a port the OS assigns, hands the browser
//! only `http://127.0.0.1:<port>/`, and answers the first `GET` from this OS account
//! ([`sot_log::peer_owner::admit`]) with a `302` to the page, then closes. Another account's connection and anything
//! but a `GET` are closed unanswered and do not spend the redirect; with no browser after [`REDIRECT_TTL`] the
//! listener closes. A remote frontend's page proxy (`proxy_listen.rs`) already holds the page's port when this runs,
//! so one `Location` serves a local and a remote frontend.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use sot_log::peer_owner::Admit;

/// How long an unclaimed redirect waits for its browser: a cold browser start, not a session.
pub const REDIRECT_TTL: Duration = Duration::from_secs(120);
/// How long one admitted connection may take to send its request head.
const REQUEST_WAIT: Duration = Duration::from_secs(2);
/// The largest request head read; a browser's GET is far smaller.
const HEAD_MAX: usize = 8192;

/// Open `page`, an http(s) address that may carry a secret, in this machine's OS browser through a one-use redirect.
pub fn open_page(page: &str) -> std::io::Result<()> {
    open_page_with(page, sot_log::peer_owner::admit, REDIRECT_TTL, spawn_opener)
}

fn open_page_with(
    page: &str,
    admit: Admit,
    ttl: Duration,
    opener: impl FnOnce(&str) -> std::io::Result<()>,
) -> std::io::Result<()> {
    if !(page.starts_with("http://") || page.starts_with("https://"))
        || page.chars().any(|c| c.is_control() || c == ' ')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "page address refused: not http(s), or it carries a space or control character",
        ));
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let at = listener.local_addr()?;
    let page = page.to_string();
    std::thread::Builder::new()
        .name("page-open".into())
        .spawn(move || redirect_once(listener, &page, admit, Instant::now() + ttl))?;
    opener(&format!("http://{at}/"))
}

/// Serve the redirect until it is spent or `deadline` passes; the listener closes when this returns.
fn redirect_once(listener: TcpListener, page: &str, admit: Admit, deadline: Instant) {
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, peer)) => {
                if answer(stream, peer, page, admit) {
                    return;
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    tracing::info!(port, "page-open: no browser came for the page; its redirect closed");
}

/// One connection; true once the redirect was written, which spends it.
fn answer(mut stream: TcpStream, peer: SocketAddr, page: &str, admit: Admit) -> bool {
    let Ok(local) = stream.local_addr() else { return false };
    if !admit("page-open", local, peer) {
        return false; // another account, or one the lookup could not name: closed with no byte read or written
    }
    // An accepted socket inherits non-blocking mode on macOS and Windows.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(REQUEST_WAIT)).is_err()
        || stream.set_write_timeout(Some(REQUEST_WAIT)).is_err()
    {
        return false;
    }
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        if head.len() >= HEAD_MAX {
            return false;
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return false,
            Ok(n) => head.extend_from_slice(&buf[..n]),
        }
    }
    if !head.starts_with(b"GET ") {
        return false;
    }
    stream.write_all(redirect(page).as_bytes()).and_then(|()| stream.flush()).is_ok()
}

/// The redirect: `Location` is the page; nothing caches it, and the page's first request names no referrer.
fn redirect(page: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {page}\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// Hand `arg` to this machine's OS opener, fire-and-forget. `arg` lands on the opener's and the browser's command
/// lines, which other accounts can read: pass only an address with no secret in it (the redirect's, a local file,
/// the public manual).
pub fn spawn_opener(arg: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        // Avoid `cmd /c start`: shell metacharacters in URLs, especially
        // `&secret=...` on Pluto links, are otherwise parsed by cmd.exe.
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", arg])
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(arg)
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(arg)
            .spawn()
            .map(|_| ())?;
    }
    Ok(())
}

/// What a log line or the status line may show of a page address: scheme, host and port. Never its path or query,
/// where a secret lives, nor any userinfo. An address with no `://` shows only its scheme.
pub fn origin_of(url: &str) -> String {
    match url.find("://") {
        Some(i) => {
            let rest = &url[i + 3..];
            let end = rest.find(|c: char| matches!(c, '/' | '?' | '#')).unwrap_or(rest.len());
            let authority = &rest[..end];
            let host = authority.rfind('@').map_or(authority, |k| &authority[k + 1..]);
            format!("{}://{host}", &url[..i])
        }
        None => url.split(':').next().unwrap_or("").to_string(),
    }
}

/// Whether this frontend opens a `browser` frame: every frontend when `open`; otherwise only the one whose address
/// is `fe`, the exact match a directed `fe.command` gets (`route_fe_command`).
pub fn opens_here(open: bool, fe: Option<&str>, me: &str) -> bool {
    open || fe == Some(me)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mine(_: &'static str, _: SocketAddr, _: SocketAddr) -> bool {
        true
    }
    fn stranger(_: &'static str, _: SocketAddr, _: SocketAddr) -> bool {
        false
    }

    const PAGE: &str = "http://127.0.0.1:9/0123456789abcdef0123456789abcdef?secret=s3cr3t&id=1";

    /// Open `PAGE` with `admit`; return the address the opener was handed.
    fn handed(admit: Admit, ttl: Duration) -> String {
        let mut got = String::new();
        open_page_with(PAGE, admit, ttl, |a| {
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

    #[test]
    fn the_browser_is_handed_no_secret() {
        let a = handed(mine, Duration::from_secs(5));
        let port = a.strip_prefix("http://127.0.0.1:").and_then(|r| r.strip_suffix('/')).unwrap_or("");
        assert!(!port.is_empty() && port.chars().all(|c| c.is_ascii_digit()), "{a}");
        assert!(!a.contains("0123456789abcdef"), "{a}");
        assert!(!a.contains("secret"), "{a}");
    }

    #[test]
    fn the_first_get_from_this_account_is_redirected_then_the_redirect_closes() {
        let a = handed(mine, Duration::from_secs(5));
        let reply = get(&a, GET);
        assert!(reply.starts_with("HTTP/1.1 302 "), "{reply}");
        assert!(reply.contains(&format!("\r\nLocation: {PAGE}\r\n")), "{reply}");
        assert!(reply.contains("\r\nReferrer-Policy: no-referrer\r\n"), "{reply}");
        assert!(reply.contains("\r\nCache-Control: no-store\r\n"), "{reply}");
        assert!(refused_within(&a, Duration::from_secs(2)), "the spent redirect still listens");
    }

    #[test]
    fn another_accounts_connection_is_closed_unanswered() {
        let a = handed(stranger, Duration::from_millis(500));
        assert_eq!(get(&a, GET), "");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(TcpStream::connect(host_port(&a)).is_err(), "the expired redirect still listens");
    }

    #[test]
    fn only_a_get_spends_the_redirect() {
        let a = handed(mine, Duration::from_secs(5));
        assert_eq!(get(&a, b"POST / HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n"), "");
        assert!(get(&a, GET).starts_with("HTTP/1.1 302 "));
    }

    #[test]
    fn an_address_that_could_forge_a_header_or_a_scheme_is_refused() {
        for bad in ["http://127.0.0.1:9/x\r\nSet-Cookie: a=b", "file:///etc/passwd", "javascript:alert(1)"] {
            let err = open_page_with(bad, mine, Duration::from_secs(1), |_| panic!("opener called for {bad:?}"))
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
}
