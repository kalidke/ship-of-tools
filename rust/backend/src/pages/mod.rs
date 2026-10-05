//! Loopback HTTP pages and the proxy that reaches them (the video server and
//! `proxy.connect`). Part of the daemon's page serving; see CLAUDE.md here.

mod http;
pub(super) mod ops;
pub(super) mod proxy;
pub(super) mod site;
pub(super) mod video;

/// A loopback listener on `port` for a page server (0: the OS picks). Bound through std, whose Windows sockets are
/// made non-inheritable at creation, and handed to tokio, whose own sockets are inheritable: a child process the daemon
/// starts must not hold a page listener (ADR 0049, User isolation). Call it inside the runtime.
pub(crate) fn bind_page_listener(port: u16) -> std::io::Result<tokio::net::TcpListener> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    listener.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(listener)
}

/// Binds the video, static-site and site-pool servers at boot; a failed bind is logged and the daemon runs on.
pub(crate) async fn start_page_servers() {
    // Loopback video file server for browser playback (ADR 0018). Bound at
    // startup so `video.open` URLs are immediately reachable. Prefers
    // `video_port()`, falls back to an ephemeral port when it's taken
    // (another user's daemon on a shared host); URLs and the ADR-0035 proxy
    // allowlist follow the ACTUAL port. Serves only video files, 127.0.0.1
    // only. The warn below now fires only when even the ephemeral bind fails
    // — an exhausted-ports / broken-loopback host, not the collision class.
    if let Err(e) = crate::pages::video::spawn(crate::pages::video::video_port()).await {
        tracing::warn!(error = %e, "video http server failed to start; `o` on a video won't work");
    }

    // Loopback static-site server (ADR 0024). Serves ANY on-disk static site —
    // its root is set per-open by the `docs.open` handler to the cursored file's
    // directory — so `W` opens whatever site/page is selected (HTML/CSS/JS/assets/
    // sub-paths) in the OS browser with full fidelity. Same preferred-then-
    // ephemeral bind story as the video server above. 127.0.0.1 only;
    // workspace-agnostic.
    if let Err(e) = crate::pages::site::spawn(crate::pages::site::site_port()).await {
        tracing::warn!(error = %e, "static-site server failed to start; `W` won't work");
    }
    // ADR 0029 Option B: the dedicated-port pool for root-relative sites
    // (an example project's __site etc.). Taken range ports fall back to
    // ephemeral ones; only a failed ephemeral bind shrinks the pool —
    // docs.open reports "slots busy" when none are assignable.
    crate::pages::site::spawn_pool().await;
}

/// Generate an unguessable token (32 lowercase hex chars = 128 bits from the
/// OS CSPRNG), or `None` if the OS CSPRNG can't be read. Fails CLOSED
/// (security review): a predictable token defeats the whole point of this
/// scheme, so callers must refuse to mint a grant rather than fall back to
/// something merely "unpredictable-ish".
fn random_token() -> Option<String> {
    let mut buf = [0u8; 16];
    if let Err(e) = getrandom::fill(&mut buf) {
        tracing::error!(error = %e, "random_token: OS CSPRNG read failed — refusing to mint a predictable token");
        return None;
    }
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(all(test, windows))]
mod bind_tests {
    /// ADR 0049, User isolation: a page listener is not inherited by the daemon's child processes.
    #[tokio::test]
    async fn a_page_listener_is_not_inheritable() {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
        let listener = super::bind_page_listener(0).unwrap();
        let mut flags = 0u32;
        let ok = unsafe { GetHandleInformation(listener.as_raw_socket() as _, &mut flags) };
        assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        assert_eq!(flags & HANDLE_FLAG_INHERIT, 0, "the page listener's socket is inheritable");
    }
}
