//! Opening a served page in the OS browser without its address on any command line (decision 0031). A page address
//! can carry a secret (`wglshow`'s path, Pluto's `?secret=`, a docs or video nonce), and on Linux every process's
//! arguments are readable by other accounts (`/proc/<pid>/cmdline`), so neither the opener nor the browser it starts
//! may be handed one. [`open_page`] binds a loopback listener of its own on a port the OS assigns, hands the browser
//! only `http://127.0.0.1:<port>/`, and answers the first `GET` from this OS account
//! ([`sot_log::identity::peer_owner::serve_own`]) with a `302` to the page, then closes. Another account's connection
//! is closed before a byte is read, and anything but a `GET` is closed unanswered and does not spend the redirect; with
//! no browser after [`REDIRECT_TTL`] the listener closes. A remote frontend's page proxy (`pages.rs`) already holds the
//! page's port when this runs, so one `Location` serves a local and a remote frontend.

use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How long an unclaimed redirect waits for its browser: a cold browser start, not a session.
pub const REDIRECT_TTL: Duration = Duration::from_secs(120);
/// How long one admitted connection may take to send its request head, and again to take the reply.
const REQUEST_WAIT: Duration = Duration::from_secs(2);
/// The largest request head read; a browser's GET is far smaller.
const HEAD_MAX: usize = 8192;

/// Open `page`, an http(s) address that may carry a secret, in this machine's OS browser through a one-use redirect.
pub fn open_page(page: &str) -> std::io::Result<()> {
    open_page_with(page, REDIRECT_TTL, spawn_opener)
}

fn open_page_with(page: &str, ttl: Duration, opener: impl FnOnce(&str) -> std::io::Result<()>) -> std::io::Result<()> {
    if !(page.starts_with("http://") || page.starts_with("https://"))
        || page.chars().any(|c| c.is_control() || c == ' ')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "page address refused: not http(s), or it carries a space or control character",
        ));
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let at = listener.local_addr()?;
    let page = page.to_string();
    std::thread::Builder::new()
        .name("page-open".into())
        .spawn(move || redirect_once(listener, page, ttl))?;
    opener(&format!("http://{at}/"))
}

/// Serve the redirect on a runtime of this thread until it is spent or `ttl` passes; the listener closes when this
/// returns.
fn redirect_once(listener: std::net::TcpListener, page: String, ttl: Duration) {
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
    rt.block_on(async move {
        let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
        let spent = Arc::new(tokio::sync::Notify::new());
        let claimed = Arc::new(AtomicBool::new(false));
        let page = Arc::new(page);
        let handle = {
            let spent = Arc::clone(&spent);
            move |stream| {
                let (spent, claimed, page) = (Arc::clone(&spent), Arc::clone(&claimed), Arc::clone(&page));
                async move {
                    if answer(stream, &page, &claimed).await {
                        spent.notify_one();
                    }
                }
            }
        };
        tokio::select! {
            () = sot_log::identity::peer_owner::serve_own(listener, "page-open", handle) => {}
            () = spent.notified() => {}
            () = tokio::time::sleep(ttl) => {
                tracing::info!(port, "page-open: no browser came for the page; its redirect closed");
            }
        }
    });
}

/// One admitted connection; true once the redirect was written, which spends it. `claimed` lets only the first
/// connection with a `GET` write it.
async fn answer(mut stream: tokio::net::TcpStream, page: &str, claimed: &AtomicBool) -> bool {
    let read_head = async {
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            if head.len() >= HEAD_MAX {
                return false;
            }
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return false,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        head.starts_with(b"GET ")
    };
    if !tokio::time::timeout(REQUEST_WAIT, read_head).await.unwrap_or(false) || claimed.swap(true, SeqCst) {
        return false;
    }
    let write = async {
        stream.write_all(redirect(page).as_bytes()).await?;
        stream.flush().await
    };
    let written = tokio::time::timeout(REQUEST_WAIT, write).await.is_ok_and(|r| r.is_ok());
    if !written {
        claimed.store(false, SeqCst); // the browser did not get it: the redirect is still unspent
    }
    written
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
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Instant;

    const PAGE: &str = "http://127.0.0.1:9/0123456789abcdef0123456789abcdef?secret=s3cr3t&id=1";

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

    #[test]
    fn the_browser_is_handed_no_secret() {
        let a = handed(Duration::from_secs(5));
        let port = a.strip_prefix("http://127.0.0.1:").and_then(|r| r.strip_suffix('/')).unwrap_or("");
        assert!(!port.is_empty() && port.chars().all(|c| c.is_ascii_digit()), "{a}");
        assert!(!a.contains("0123456789abcdef"), "{a}");
        assert!(!a.contains("secret"), "{a}");
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
}
