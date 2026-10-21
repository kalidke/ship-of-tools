//! Opening a served page in the OS browser without its address on any command line (decision 0031). A page address
//! can carry a secret (`wglshow`'s path, Pluto's `?secret=`, a docs or video nonce), and on Linux every process's
//! arguments are readable by other accounts (`/proc/<pid>/cmdline`), so neither the opener nor the browser it starts
//! may be handed one. [`open_page`] binds a loopback listener of its own on a port the OS assigns, hands the browser
//! only `http://127.0.0.1:<port>/`, and answers the first `GET` from this OS account
//! ([`sot_log::identity::peer_owner::serve_own`]) with a `302` to the page, then closes. Another account's connection
//! is closed before a byte is read, and anything but a `GET` is closed unanswered and does not spend the redirect; with
//! no browser after [`REDIRECT_TTL`] the listener closes. A remote frontend's page proxy (`pages.rs`) already holds the
//! page's port when this runs, and the address it is handed is the one its caller opens: the daemon's on a local
//! frontend, the page proxy's on a remote one (`ensure_proxy_for_url`).

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
        #[allow(clippy::disallowed_methods, reason = "the window hands the address to the OS browser, which outlives the window by design")]
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", arg])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "linux")]
    {
        #[allow(clippy::disallowed_methods, reason = "the window hands the address to the OS browser, which outlives the window by design")]
        std::process::Command::new("xdg-open")
            .arg(arg)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())?;
    }
    #[cfg(target_os = "macos")]
    {
        #[allow(clippy::disallowed_methods, reason = "the window hands the address to the OS browser, which outlives the window by design")]
        std::process::Command::new("open")
            .arg(arg)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
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
#[path = "browser_open_tests.rs"]
mod tests;
