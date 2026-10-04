//! Loopback HTTP pages and the proxy that reaches them (the video server and
//! `proxy.connect`). Part of the daemon's page serving; see CLAUDE.md here.

mod http;
pub(super) mod ops;
pub(super) mod proxy;
pub(super) mod site;
pub(super) mod video;

/// Binds the video, static-site and site-pool servers at boot; a failed bind is logged and the daemon runs on.
pub(crate) async fn start_page_servers() {
    // Loopback video file server for browser playback (ADR 0018). Bound at
    // startup so `video.open` URLs are immediately reachable. Prefers
    // `video_port()`, falls back to an ephemeral port when it's taken
    // (another user's daemon on a shared host); URLs and the ADR-0035 proxy
    // allowlist follow the ACTUAL port. Serves only video files, 127.0.0.1
    // only. The warn below now fires only when even the ephemeral bind fails
    // — an exhausted-ports / broken-loopback host, not the collision class.
    if let Err(e) = crate::http_serve::spawn(crate::http_serve::video_port()).await {
        tracing::warn!(error = %e, "video http server failed to start; `o` on a video won't work");
    }

    // Loopback static-site server (ADR 0024). Serves ANY on-disk static site —
    // its root is set per-open by the `docs.open` handler to the cursored file's
    // directory — so `W` opens whatever site/page is selected (HTML/CSS/JS/assets/
    // sub-paths) in the OS browser with full fidelity. Same preferred-then-
    // ephemeral bind story as the video server above. 127.0.0.1 only;
    // workspace-agnostic.
    if let Err(e) = crate::site_serve::spawn(crate::site_serve::site_port()).await {
        tracing::warn!(error = %e, "static-site server failed to start; `W` won't work");
    }
    // ADR 0029 Option B: the dedicated-port pool for root-relative sites
    // (an example project's __site etc.). Taken range ports fall back to
    // ephemeral ones; only a failed ephemeral bind shrinks the pool —
    // docs.open reports "slots busy" when none are assignable.
    crate::site_serve::spawn_pool().await;
}
