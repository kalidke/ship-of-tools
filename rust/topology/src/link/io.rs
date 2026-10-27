//! The link's one reader and one writer, and its silence rule. The reader never waits on a channel's consumer: a
//! channel's bytes go to a queue sized to the credit its end granted. The writer sends the link ops first, then each
//! channel with data and credit in turn, one chunk each.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use sot_protocol::ops::link::{LinkCloseEvt, LinkCreditEvt, LinkDataEvt, LinkOpenReq, LinkOpenRes, LinkRefusal, CHUNK};
use sot_protocol::{codec, op, BlobDescriptor, Frame, Kind};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWrite, ReadBuf};
use tokio::time::Instant;

use super::channel::Channel;
use super::table::{End, Op, Out, Shared};

/// A connection the router makes for a channel: any byte stream both ways.
pub trait Duplex: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Duplex for T {}

/// How the router connects a channel to its daemon.
pub type Connector = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = std::io::Result<Box<dyn Duplex>>> + Send>> + Send + Sync>;

/// A reader that notes when its last byte came: what the silence rule reads.
pub(super) struct Heard<R> {
    inner: R,
    last: Arc<Mutex<Instant>>,
}

impl<R> Heard<R> {
    pub(super) fn new(inner: R) -> (Self, Arc<Mutex<Instant>>) {
        let last = Arc::new(Mutex::new(Instant::now()));
        (Self { inner, last: Arc::clone(&last) }, last)
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Heard<R> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            *self.last.lock().unwrap_or_else(|p| p.into_inner()) = Instant::now();
        }
        polled
    }
}

/// Runs a link until it ends: its reader, its writer and its silence rule, each of which can end it, and an end from
/// outside (a dropped link, a violation a channel's user found). The reason is the table's.
pub(super) async fn run<R, W>(shared: Arc<Shared>, rx: R, last: Arc<Mutex<Instant>>, tx: W, connector: Option<Connector>, silence: Duration)
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let end = tokio::select! {
        end = read_loop(&shared, rx, connector) => end,
        end = write_loop(&shared, tx) => end,
        end = watch_silence(last, silence) => end,
        () = shared.wait_ended() => return,
    };
    shared.end(end);
}

async fn watch_silence(last: Arc<Mutex<Instant>>, silence: Duration) -> End {
    loop {
        let at = *last.lock().unwrap_or_else(|p| p.into_inner()) + silence;
        if Instant::now() >= at {
            return End::failed(format!("nothing from the far end in {silence:?}"));
        }
        tokio::time::sleep_until(at).await;
    }
}

/// The router's channels are tasks this loop owns: when the link ends, its select drops the loop and with it every
/// channel's connect and pump, whatever each is waiting on, and each local connection closes.
async fn read_loop<R: AsyncBufRead + Unpin>(shared: &Arc<Shared>, mut rx: R, connector: Option<Connector>) -> End {
    let mut channels = tokio::task::JoinSet::new();
    loop {
        while channels.try_join_next().is_some() {}
        let frame = match codec::read_envelope(&mut rx).await {
            Ok(frame) => frame,
            Err(e) if e.to_string() == "eof" => return End::clean("the far end closed the link"),
            Err(e) => return End::failed(format!("reading the link: {e:#}")),
        };
        match on_frame(shared, &mut rx, frame, connector.as_ref(), &mut channels).await {
            Ok(None) => {}
            Ok(Some(end)) => return end,
            Err(violation) => return End::failed(violation),
        }
    }
}

fn payload<T: serde::de::DeserializeOwned>(frame: &Frame) -> Result<T, String> {
    serde_json::from_value(frame.payload.clone()).map_err(|e| format!("a {} frame that does not parse: {}", frame.op, codec::unparsed(&e, 0)))
}

/// One frame from the peer. `Err` is a violation of the link's rules; `Ok(Some)` an end the peer asked for.
async fn on_frame<R: AsyncBufRead + Unpin>(
    shared: &Arc<Shared>,
    rx: &mut R,
    frame: Frame,
    connector: Option<&Connector>,
    channels: &mut tokio::task::JoinSet<()>,
) -> Result<Option<End>, String> {
    let declared = codec::declared_blob_len(&frame);
    match (frame.kind, frame.op.as_str()) {
        (Kind::Evt, op::LINK_DATA) => {
            let data: LinkDataEvt = payload(&frame)?;
            let len = declared.unwrap_or(0);
            // Refused before a byte of it is read: a blob over the chunk is the peer breaking the rules.
            if len > CHUNK as u64 {
                return Err(format!("a blob of {len} bytes on channel {}, over the {CHUNK}-byte chunk", data.ch));
            }
            let mut bytes = Vec::with_capacity(len as usize);
            (&mut *rx).take(len).read_to_end(&mut bytes).await.map_err(|e| format!("reading a channel's bytes: {e}"))?;
            if (bytes.len() as u64) < len {
                return Err(format!("the link ended after {} of {len} bytes", bytes.len()));
            }
            shared.with(|t| t.on_data(data.ch, bytes))?;
            Ok(None)
        }
        _ if declared.is_some() => Err(format!("a {} frame that declares a blob", frame.op)),
        (Kind::Evt, op::LINK_CREDIT) => {
            let credit: LinkCreditEvt = payload(&frame)?;
            shared.with(|t| t.on_credit(credit.ch, credit.bytes))?;
            Ok(None)
        }
        (Kind::Evt, op::LINK_CLOSE) => {
            let close: LinkCloseEvt = payload(&frame)?;
            let control = shared.with(|t| t.on_close(close.ch, close.reason))?;
            Ok(control.then(|| End::clean("the far end closed channel 0")))
        }
        (Kind::Res, op::LINK_OPEN) => {
            let answer = if frame.payload.get("error").is_some() {
                let refusal: LinkRefusal = payload(&frame)?;
                (refusal.ch.unwrap_or_default(), Err((refusal.code, refusal.error)))
            } else {
                let opened: LinkOpenRes = payload(&frame)?;
                if !opened.ok {
                    return Err(format!("an answer to the open of channel {} that is neither ok nor a refusal", opened.ch));
                }
                (opened.ch, Ok(()))
            };
            shared.with(|t| t.on_open_answer(answer.0, answer.1))?;
            Ok(None)
        }
        (Kind::Req, op::LINK_OPEN) => {
            let open: LinkOpenReq = payload(&frame)?;
            let connect = shared.with(|t| t.on_open_request(open.ch, frame.id))?;
            if connect {
                let connector = connector.ok_or("a link.open sent to the window")?;
                channels.spawn(connect_channel(Arc::clone(shared), open.ch, frame.id, Arc::clone(connector)));
            }
            Ok(None)
        }
        (kind, name) => Err(format!("a {name} frame ({kind:?}) on the link")),
    }
}

/// The router's connect for one channel, then its pump; the answer to the open goes out in between.
async fn connect_channel(shared: Arc<Shared>, ch: u32, id: u64, connector: Connector) {
    let result = connector().await;
    let accepted = shared.with(|t| t.connected(ch, id, result.as_ref().map(|_| ()).map_err(|e| e.to_string())));
    if let (true, Ok(stream)) = (accepted, result) {
        pump(Channel::new(shared, ch), stream).await;
    }
}

/// Copies a channel and its local connection both ways until either ends; the channel then closes (an `eof` after the
/// bytes before it), and the local connection with it.
pub(super) async fn pump(channel: Channel, local: Box<dyn Duplex>) {
    use tokio::io::AsyncWriteExt;
    let (mut from_link, mut to_link) = channel.into_split();
    let (mut from_local, mut to_local) = tokio::io::split(local);
    let up = async {
        let _ = tokio::io::copy(&mut from_link, &mut to_local).await;
        let _ = to_local.shutdown().await;
    };
    let down = async {
        let _ = tokio::io::copy(&mut from_local, &mut to_link).await;
        let _ = to_link.shutdown().await;
    };
    tokio::select! {
        () = up => {}
        () = down => {}
    }
}

async fn write_loop<W: AsyncWrite + Unpin>(shared: &Arc<Shared>, mut tx: W) -> End {
    loop {
        let notified = shared.writer.notified();
        let next = {
            let mut table = shared.lock();
            if let Some(end) = table.down() {
                return end.clone();
            }
            table.next_out()
        };
        let Some(out) = next else {
            notified.await;
            continue;
        };
        // A frame taken may have freed room for a writer or credit for another frame.
        shared.changed();
        let (frame, blob, ends) = match out {
            Out::Op(Op::Open { ch, id }) => (Frame::req(id, op::LINK_OPEN, json(LinkOpenReq { ch })), None, false),
            Out::Op(Op::Opened { ch, id }) => (Frame::res(id, op::LINK_OPEN, json(LinkOpenRes { ch, ok: true })), None, false),
            Out::Op(Op::Refused { ch, id, code, error }) => {
                let refusal = LinkRefusal { error, code: code.to_string(), ch: Some(ch), link: None };
                (Frame::res(id, op::LINK_OPEN, json(refusal)), None, false)
            }
            Out::Op(Op::Credit { ch, bytes }) => (Frame::evt(op::LINK_CREDIT, json(LinkCreditEvt { ch, bytes })), None, false),
            Out::Data { ch, bytes } => {
                let blob = BlobDescriptor { len: bytes.len() as u64, mime: "application/octet-stream".to_string() };
                (Frame::evt(op::LINK_DATA, json(LinkDataEvt { ch, blob })), Some(bytes), false)
            }
            Out::Close { ch, reason } => (Frame::evt(op::LINK_CLOSE, json(LinkCloseEvt { ch, reason })), None, ch == 0),
        };
        if let Err(e) = codec::write_frame(&mut tx, &frame, blob.as_deref()).await {
            return End::failed(format!("writing the link: {e:#}"));
        }
        if ends {
            return End::clean("channel 0 closed");
        }
    }
}

fn json(value: impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a link payload serializes")
}
