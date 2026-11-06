//! The fake's websocket: the selected library's server handshake over the fake's TLS, then a test's script drives the
//! connection through a `WsPeer`. The fake counts connections open, the most open at once and all accepted, so a test
//! can prove that no two sockets of the client ever overlapped. A socket counts as open until its script ends and drops
//! the peer, which closes the connection.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use futures_util::{SinkExt, StreamExt};

pub(super) type SocketScript =
    Arc<dyn Fn(WsPeer) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Default)]
struct Counts {
    open: AtomicUsize,
    peak: AtomicUsize,
    total: AtomicUsize,
}

#[derive(Default)]
pub(super) struct Sockets(Arc<Counts>);

impl Sockets {
    pub(super) fn enter(&self) -> OpenGuard {
        let now = self.0.open.fetch_add(1, Ordering::SeqCst) + 1;
        self.0.peak.fetch_max(now, Ordering::SeqCst);
        self.0.total.fetch_add(1, Ordering::SeqCst);
        OpenGuard(self.0.clone())
    }

    pub(super) fn counts(&self) -> (usize, usize, usize) {
        (
            self.0.open.load(Ordering::SeqCst),
            self.0.peak.load(Ordering::SeqCst),
            self.0.total.load(Ordering::SeqCst),
        )
    }
}

pub(super) struct OpenGuard(Arc<Counts>);

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The bytes already read from the stream, then the stream: the handshake reads the request the fake consumed.
struct Replay<S> {
    head: Vec<u8>,
    at: usize,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Replay<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.at < self.head.len() {
            let n = (self.head.len() - self.at).min(buf.remaining());
            let from = self.at;
            buf.put_slice(&self.head[from..from + n]);
            self.at += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Replay<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

type Inner = Replay<TlsStream<TcpStream>>;

/// How `send_until_dropped` ended.
#[derive(Debug)]
pub(crate) enum Flood {
    /// The client dropped the connection: how long after the first stalled send, if any send stalled.
    Dropped(Option<Duration>),
    /// The frame or time cap came first with the connection still up: frames sent, and whether a send stalled.
    Held { frames: usize, stalled: bool },
}

/// The server side of one websocket connection, for a script to drive.
pub(crate) struct WsPeer {
    ws: WebSocketStream<Inner>,
    _open: OpenGuard,
}

pub(super) async fn serve_socket(
    tls: TlsStream<TcpStream>,
    raw: Vec<u8>,
    script: SocketScript,
    open: OpenGuard,
) {
    let stream = Replay {
        head: raw,
        at: 0,
        inner: tls,
    };
    let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };
    script(WsPeer { ws, _open: open }).await;
}

impl WsPeer {
    pub(crate) async fn send(&mut self, value: &Value) {
        self.send_text(value.to_string()).await;
    }

    pub(crate) async fn send_text(&mut self, text: String) {
        let _ = self.ws.send(Message::Text(text.into())).await;
    }

    /// The next text frame as JSON, within `within`. `None` on a timeout or when the client is gone.
    pub(crate) async fn recv(&mut self, within: Duration) -> Option<Value> {
        let end = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(end, self.ws.next()).await.ok()?? {
                Ok(Message::Text(text)) => return serde_json::from_str(&text).ok(),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    /// A websocket ping with a text payload, as Slack sends.
    pub(crate) async fn ping(&mut self, payload: &str) {
        let _ = self
            .ws
            .send(Message::Ping(payload.as_bytes().to_vec().into()))
            .await;
    }

    /// Read frames until a pong carrying `payload` arrives, within `within`; false if a text frame (an
    /// acknowledgement) comes first, or on a timeout or a wrong payload.
    pub(crate) async fn expect_pong_first(&mut self, payload: &str, within: Duration) -> bool {
        let end = tokio::time::Instant::now() + within;
        loop {
            match tokio::time::timeout_at(end, self.ws.next()).await {
                Ok(Some(Ok(Message::Pong(got)))) => return got.as_ref() == payload.as_bytes(),
                Ok(Some(Ok(Message::Text(_)))) => return false,
                Ok(Some(Ok(_))) => {}
                _ => return false,
            }
        }
    }

    /// Send `text` frames without ever reading, until a send fails (the client dropped the connection), `frames`
    /// are sent or `within` passes. A send that waits past `stall` is the client having stopped reading because
    /// its own write to us cannot finish; only the first such stall is recorded.
    pub(crate) async fn send_until_dropped(
        &mut self,
        text: &str,
        stall: Duration,
        frames: usize,
        within: Duration,
    ) -> Flood {
        let end = tokio::time::Instant::now() + within;
        let mut stuck = None;
        for sent in 0..frames {
            let mut send = std::pin::pin!(self.ws.send(Message::Text(text.to_string().into())));
            let mut result = None;
            if stuck.is_none() {
                tokio::select! {
                    r = &mut send => result = Some(r),
                    _ = tokio::time::sleep(stall) => stuck = Some(tokio::time::Instant::now()),
                }
            }
            let result = match result {
                Some(r) => r,
                None => match tokio::time::timeout_at(end, send).await {
                    Ok(r) => r,
                    Err(_) => {
                        return Flood::Held {
                            frames: sent,
                            stalled: stuck.is_some(),
                        }
                    }
                },
            };
            if result.is_err() {
                return Flood::Dropped(stuck.map(|at| at.elapsed()));
            }
            if tokio::time::Instant::now() >= end {
                return Flood::Held {
                    frames: sent,
                    stalled: stuck.is_some(),
                };
            }
        }
        Flood::Held {
            frames,
            stalled: stuck.is_some(),
        }
    }

    /// Read until the client ends the connection; the library answers a close.
    pub(crate) async fn drain(&mut self) {
        while let Some(Ok(_)) = self.ws.next().await {}
    }

    /// Never parse a frame, so never answer a close: read raw bytes until the client's connection is gone.
    pub(crate) async fn ignore_until_gone(&mut self) {
        let mut buf = [0u8; 1024];
        while let Ok(n) = self.ws.get_mut().read(&mut buf).await {
            if n == 0 {
                break;
            }
        }
    }
}
