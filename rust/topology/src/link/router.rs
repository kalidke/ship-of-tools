//! The far end of a window's link: `sotd stdio-bridge` runs [`serve`] when its first frame is `link.hello`. Channel 0
//! is the connection the bridge already made to its daemon; each `link.open` is one more connection there, made by
//! the bridge's connector (`connect_own`), so the daemon admits every channel as it admits any connection, by its OS
//! peer at accept and then its hello. The router reads no channel byte.

use std::sync::Arc;
use std::time::Duration;

use sot_protocol::ops::link::{LinkHelloReq, LinkHelloRes, LinkRefusal, LINK};
use sot_protocol::{codec, op, Frame, Kind};
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub use super::io::{Connector, Duplex};
use super::table::{Shared, Side};

/// How the link ended: `Ok` for an end either side meant (the window closed it, or channel 0 ended), else why.
pub type Served = Result<(), String>;

/// Serves one link: answers `first` (the `link.hello` the bridge read), then carries channel 0 over `control` and
/// each channel the window opens over a connection `connect` makes, until the link ends: the window's end of input,
/// channel 0's end on either side, a broken rule, or `SILENCE_BOUND` with no byte from the window. Every local
/// connection ends with it.
pub async fn serve<R, W>(first: Frame, rx: R, tx: W, control: Box<dyn Duplex>, connect: Connector) -> Served
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    serve_within(first, rx, tx, control, connect, super::SILENCE_BOUND).await
}

/// [`serve`] with the silence bound given: `sotd stdio-bridge` passes its test override, every other caller
/// `SILENCE_BOUND`.
pub async fn serve_within<R, W>(first: Frame, rx: R, mut tx: W, control: Box<dyn Duplex>, connect: Connector, silence: Duration) -> Served
where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let hello = (first.kind == Kind::Req && first.op == op::LINK_HELLO).then(|| serde_json::from_value::<LinkHelloReq>(first.payload.clone()));
    let asked = match hello {
        Some(Ok(hello)) => hello.link,
        _ => return Err(format!("the first frame is not a link.hello ({} {:?})", first.op, first.kind)),
    };
    if asked != LINK {
        let refusal = LinkRefusal { error: format!("this router speaks link {LINK}, the window link {asked}"), code: "link_mismatch".to_string(), ch: None, link: Some(LINK) };
        let _ = answer(&mut tx, first.id, Some(refusal)).await;
        return Err(format!("the window speaks link {asked}, this router link {LINK}"));
    }
    answer(&mut tx, first.id, None).await.map_err(|e| format!("answering link.hello: {e}"))?;
    let (heard, last) = super::io::Heard::new(rx);
    let rx = tokio::io::BufReader::new(heard);
    let shared = Shared::new(Side::Router);
    let pump = tokio::spawn(super::io::pump(super::channel::Channel::new(Arc::clone(&shared), 0), control));
    super::io::run(Arc::clone(&shared), rx, last, tx, Some(connect), silence).await;
    pump.abort();
    let end = shared.lock().down().cloned().expect("a link that returned has ended");
    if end.clean {
        Ok(())
    } else {
        Err(end.reason)
    }
}

/// Refuses a link the router cannot carry channel 0 for: its daemon did not take the connection. The window reads it
/// as a failed start and retries on its pace.
pub async fn refuse_unreachable<W: AsyncWrite + Unpin>(first: &Frame, tx: &mut W, why: &str) {
    let refusal = LinkRefusal { error: why.to_string(), code: "unreachable".to_string(), ch: None, link: Some(LINK) };
    let _ = answer(tx, first.id, Some(refusal)).await;
}

/// Whether `frame` is a link's first frame: the bridge routes on it.
pub fn is_link_hello(frame: &Frame) -> bool {
    frame.kind == Kind::Req && frame.op == op::LINK_HELLO
}

async fn answer<W: AsyncWrite + Unpin>(tx: &mut W, id: u64, refusal: Option<LinkRefusal>) -> std::io::Result<()> {
    let payload = match refusal {
        Some(refusal) => serde_json::to_value(refusal),
        None => serde_json::to_value(LinkHelloRes { link: LINK, build: sot_protocol::app_version() }),
    }
    .expect("a hello answer serializes");
    codec::write_frame(tx, &Frame::res(id, op::LINK_HELLO, payload), None).await.map_err(std::io::Error::other)?;
    tx.flush().await
}
