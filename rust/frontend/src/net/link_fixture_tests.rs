//! A window's link to a far router in this process, for the window's tests: the router's channel 0 and each channel it
//! opens reach what `connect` makes, as `sotd stdio-bridge` connects them to its daemon.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sot_topology::link::router::{Connector, Duplex};
use sot_topology::link::{Link, LinkHandle};

/// A connector to the local endpoint at `path` (a stand-in daemon's socket or pipe), counting its connections.
pub(crate) fn to_endpoint(path: std::path::PathBuf, made: Arc<AtomicUsize>) -> Connector {
    Arc::new(move || {
        let (path, made) = (path.clone(), made.clone());
        Box::pin(async move {
            made.fetch_add(1, Ordering::SeqCst);
            sot_topology::link::connect_local(&path).await.map(|s| Box::new(s) as Box<dyn Duplex>)
        })
    })
}

/// Serves one link over `far` as the far bridge does: its hello, then channel 0 over `control` and each opened channel
/// over a connection `connect` makes.
pub(crate) async fn route<S>(far: S, control: Box<dyn Duplex>, connect: Connector)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
{
    let (rx, tx) = tokio::io::split(far);
    let mut rx = tokio::io::BufReader::new(rx);
    if let Ok(first) = sot_protocol::codec::read_envelope(&mut rx).await {
        let _ = sot_topology::link::router::serve(first, rx, tx, control, connect).await;
    }
}

/// A link to a router in this process whose channel 0 is `control` and whose channels `connect` makes, with the handle
/// a session would hold up.
pub(crate) async fn link_over(control: Box<dyn Duplex>, connect: Connector) -> (Link, LinkHandle) {
    let (near, far) = tokio::io::duplex(1 << 20);
    tokio::spawn(route(far, control, connect));
    let (rx, tx) = tokio::io::split(near);
    let link = Link::over(rx, tx).await.expect("the link starts");
    let handle = LinkHandle::default();
    handle.set_up(&link);
    (link, handle)
}

/// [`link_over`] with a channel 0 that says nothing and channels to `path`.
pub(crate) async fn link_to(path: std::path::PathBuf, made: Arc<AtomicUsize>) -> (Link, LinkHandle) {
    let (control, silent) = tokio::io::duplex(1 << 16);
    // Held open for the link's life: channel 0's end would end the link.
    tokio::spawn(async move {
        let _held = silent;
        std::future::pending::<()>().await
    });
    link_over(Box::new(control), to_endpoint(path, made)).await
}
