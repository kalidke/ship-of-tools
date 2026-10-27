//! A channel's two faces over the same queues: `AsyncRead` and `AsyncWrite` for the window's tasks and the router's
//! pumps, and platform's blocking `Client` for the attach worker's thread. Its last handle's drop closes it.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use sot_platform::transport::{Client, TransportError};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::table::{Failure, Read, Shared, Write};

/// One channel of a link. Clones of its halves share it; the last one dropped closes it (`eof` when nothing it wrote
/// is still waiting, `reset` otherwise).
pub struct Channel {
    inner: Arc<Held>,
}

/// The read half of a [`Channel`].
pub struct ChannelReader {
    inner: Arc<Held>,
}

/// The write half of a [`Channel`].
pub struct ChannelWriter {
    inner: Arc<Held>,
}

struct Held {
    shared: Arc<Shared>,
    ch: u32,
}

impl Drop for Held {
    fn drop(&mut self) {
        let ch = self.ch;
        self.shared.with(|t| t.release(ch));
    }
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Channel({})", self.inner.ch)
    }
}

impl Channel {
    pub(super) fn new(shared: Arc<Shared>, ch: u32) -> Self {
        Self { inner: Arc::new(Held { shared, ch }) }
    }

    /// The channel's number on its link; 0 is the control channel.
    pub fn id(&self) -> u32 {
        self.inner.ch
    }

    pub fn into_split(self) -> (ChannelReader, ChannelWriter) {
        (ChannelReader { inner: Arc::clone(&self.inner) }, ChannelWriter { inner: self.inner })
    }
}

impl Held {
    fn poll_read(&self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let ch = self.ch;
        let read = self.shared.with(|t| t.read(ch, buf.initialize_unfilled(), Some(cx.waker())));
        match read {
            Read::Bytes(n) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Read::Eof => Poll::Ready(Ok(())),
            Read::Failed(failure) => Poll::Ready(Err(failure.into_io())),
            Read::Pending => Poll::Pending,
        }
    }

    fn poll_write(&self, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
        let ch = self.ch;
        match self.shared.with(|t| t.write(ch, data, Some(cx.waker()))) {
            Write::Accepted(n) => Poll::Ready(Ok(n)),
            Write::Failed(failure) => Poll::Ready(Err(failure.into_io())),
            Write::Pending => Poll::Pending,
        }
    }

    fn poll_flush(&self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let ch = self.ch;
        if self.shared.with(|t| t.flushed(ch, Some(cx.waker()))) {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    /// Waits for what was written to reach the writer, then closes this end: the close follows those bytes.
    fn poll_shutdown(&self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        if self.poll_flush(cx).is_pending() {
            return Poll::Pending;
        }
        let ch = self.ch;
        self.shared.with(|t| t.shutdown(ch));
        Poll::Ready(Ok(()))
    }

    /// Blocks until `f` is not pending, waiting on the link's changes in between.
    fn block<T>(&self, mut f: impl FnMut(&mut super::table::Table) -> Option<T>) -> T {
        let mut table = self.shared.lock();
        loop {
            if let Some(out) = f(&mut table) {
                drop(table);
                self.shared.changed();
                return out;
            }
            table = self.shared.changed.wait(table).unwrap_or_else(|p| p.into_inner());
        }
    }
}

fn transport(failure: Failure, op: &'static str) -> TransportError {
    match failure {
        Failure::LinkDown(_) => TransportError::LinkDown,
        Failure::Cancelled => TransportError::Cancelled,
        closed => TransportError::Io { op, source: closed.into_io() },
    }
}

macro_rules! async_faces {
    ($read:ty, $write:ty) => {
        impl AsyncRead for $read {
            fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
                self.inner.poll_read(cx, buf)
            }
        }

        impl AsyncWrite for $write {
            fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<std::io::Result<usize>> {
                self.inner.poll_write(cx, data)
            }

            fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                self.inner.poll_flush(cx)
            }

            fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                self.inner.poll_shutdown(cx)
            }
        }
    };
}

async_faces!(Channel, Channel);
async_faces!(ChannelReader, ChannelWriter);

/// The blocking face: the attach worker's thread reads and writes a row's bytes through it, and another thread may
/// cancel both.
impl Client for Channel {
    fn write_all(&self, bytes: &[u8]) -> Result<(), TransportError> {
        if bytes.is_empty() {
            return Err(TransportError::EmptyPayload);
        }
        let ch = self.inner.ch;
        let mut sent = 0;
        while sent < bytes.len() {
            let wrote = self.inner.block(|t| match t.write(ch, &bytes[sent..], None) {
                Write::Accepted(n) => Some(Ok(n)),
                Write::Failed(failure) => Some(Err(failure)),
                Write::Pending => None,
            });
            sent += wrote.map_err(|f| transport(f, "link write"))?;
        }
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let ch = self.inner.ch;
        self.inner
            .block(|t| match t.read(ch, buf, None) {
                Read::Bytes(n) => Some(Ok(n)),
                Read::Eof => Some(Ok(0)),
                Read::Failed(failure) => Some(Err(failure)),
                Read::Pending => None,
            })
            .map_err(|f| transport(f, "link read"))
    }

    fn cancel(&self) {
        let ch = self.inner.ch;
        self.inner.shared.with(|t| t.cancel(ch));
    }
}
