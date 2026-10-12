//! The steady control loop: replies and pushed events, the ping, and the window's requests, with one held read and
//! one held write that progress together.

use super::*;

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::task::{Context, Poll};

/// How many ping intervals with no byte from the daemon end the connection. A live daemon answers each ping, which goes
/// out every `ping_every` that the write slot is free; the daemon reaps a window silent this long (its
/// `PING_READ_DEADLINE`, three intervals), and this is the window's half of that rule. Bytes, not frames, keep the
/// connection: a large reply still arriving is the daemon talking.
const SILENT_PINGS: u32 = 3;

/// The steady reader, stamping on the Tokio clock the last time a byte came through it.
struct Heard<R> {
    inner: R,
    since: tokio::time::Instant,
    last_ms: Arc<AtomicU64>,
}

impl<R> Heard<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            since: tokio::time::Instant::now(),
            last_ms: Arc::new(AtomicU64::new(0)),
        }
    }
}

fn stamp(since: tokio::time::Instant, last_ms: &AtomicU64) {
    last_ms.store(since.elapsed().as_millis() as u64, Relaxed);
}

/// When the reader last delivered a byte; it keeps working while the read future owns the reader.
struct LastHeard {
    since: tokio::time::Instant,
    last_ms: Arc<AtomicU64>,
}

impl LastHeard {
    fn at(&self) -> tokio::time::Instant {
        self.since + std::time::Duration::from_millis(self.last_ms.load(Relaxed))
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Heard<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if buf.filled().len() > before {
            stamp(self.since, &self.last_ms);
        }
        polled
    }
}

impl<R: tokio::io::AsyncBufRead + Unpin> tokio::io::AsyncBufRead for Heard<R> {
    fn poll_fill_buf(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<&[u8]>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_fill_buf(cx);
        if matches!(&polled, Poll::Ready(Ok(bytes)) if !bytes.is_empty()) {
            stamp(this.since, &this.last_ms);
        }
        polled
    }

    fn consume(mut self: Pin<&mut Self>, amt: usize) {
        Pin::new(&mut self.inner).consume(amt);
    }
}

/// The one encoded request or ping on its way to the daemon, written as the peer takes it. Its bytes are whole
/// frames encoded in memory, so a partly written one is resumed by the next poll and never rebuilt.
#[derive(Default)]
struct WriteSlot {
    bytes: Vec<u8>,
    sent: usize,
}

impl WriteSlot {
    fn is_idle(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Write what is left, then flush. Cancel-safe: each await either wrote and counted its bytes or wrote nothing.
    async fn advance<W: AsyncWrite + Unpin>(&mut self, tx: &mut W) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        while self.sent < self.bytes.len() {
            self.sent += tx.write(&self.bytes[self.sent..]).await?;
        }
        tx.flush().await?;
        self.bytes.clear();
        self.sent = 0;
        Ok(())
    }
}

/// Read exactly one frame while *owning* the reader, handing it back with the
/// result. This lets the steady-state select! loop keep a single in-flight
/// read future across iterations (cancel-safe: a cancelled select! pauses it
/// rather than dropping it mid-blob) without the borrow checker objecting to a
/// stored future that re-borrows `rx` each loop. See the CANCEL-SAFETY note in
/// `steady_loop`.
pub(super) async fn read_owned<R: tokio::io::AsyncBufRead + Unpin>(
    mut rx: R,
) -> (R, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// The steady-state loop: replies and pushed events, the ping, and the window's requests. It ends, naming the silence,
/// when no byte has come from the daemon for `SILENT_PINGS` ping intervals.
///
/// CANCEL-SAFETY: `read_frame` is NOT cancellation-safe — it reads the `\n`-terminated envelope and then
/// `read_exact`s the blob tail across two separate awaits, so a select! that dropped a half-read future would leave
/// blob bytes to be parsed as a JSON envelope and force a reconnect. One read future is therefore held across
/// iterations and polled by `&mut`; it owns the reader (`read_owned`) and hands it back on completion. The write is
/// held the same way: a request is encoded in memory (its pending entries entering `pending` before its first
/// network byte), kept in a [`WriteSlot`] and written as the peer takes it, while the reader stays polled, so a peer
/// that is not yet reading cannot stop the replies it owes. No next request is taken until the slot is empty, and a
/// ping waits for the slot rather than overtaking queued bytes.
pub(super) async fn steady_loop<R, W, Wn>(
    rx: tokio::io::BufReader<R>,
    mut tx: W,
    mut next_id: u64,
    pending: &mut HashMap<u64, PendingKind>,
    session: &mut SessionState,
    host: HostKey,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    out_rx: &mut UnboundedReceiver<OutgoingReq>,
    window: &Wn,
    ping_every: std::time::Duration,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    Wn: Redraw,
{
    let rx = Heard::new(rx);
    let heard = LastHeard {
        since: rx.since,
        last_ms: Arc::clone(&rx.last_ms),
    };
    let silence = ping_every * SILENT_PINGS;
    let mut read_fut = Some(Box::pin(read_owned(rx)));
    // Topology plan §F step 2 (the half-open-roster fix): this connection is always `fe`-declared, one of the
    // daemon's two long-lived roles, so it always pings. `Interval::tick` is cancellation-safe: an iteration that
    // takes another arm leaves it armed.
    let mut ping_interval = tokio::time::interval(ping_every);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping_interval.tick().await; // first tick fires immediately; consume it
    let mut slot = WriteSlot::default();
    let (mut ping_due, mut closed) = (false, false);
    loop {
        if ping_due && slot.is_idle() {
            ping_due = false;
            let id = take_id(&mut next_id);
            tracing::debug!(id, "→ ping");
            let ping = Frame::req(id, op::PING, serde_json::to_value(PingReq {})?);
            codec::write_frame(&mut slot.bytes, &ping, None).await?;
        }
        if closed && slot.is_idle() {
            // Sender side dropped — the app is shutting down, and the last request is out. Resume the in-flight
            // read (reclaiming the reader), then read plainly until the connection closes.
            tracing::debug!("outgoing channel closed; draining reads until disconnect");
            let read_fut = read_fut.take().expect("read_fut is always Some here");
            let (rx, read) = within_silence(&heard, silence, read_fut).await?;
            return drain_reads(rx, read, pending, session, &host, evt_tx, window, &heard, silence).await;
        }
        // Poll every held future; completing one never restarts another's partial frame. `biased`, in this order: the
        // ping tick, the request intake, the write, the read, and the silence deadline last. The window's own sends
        // come first, so a daemon that keeps the reader busy cannot starve them (the read would otherwise be ready on
        // every poll); they cannot starve the read, for the slot holds one frame, intake runs only while it is idle,
        // and a write goes Pending once the socket is full. The deadline is last, so a frame already readable when it
        // passes (a worker descheduled past it, a stopped process resumed) is read, not cut, as the daemon's own
        // reaper does.
        tokio::select! {
            biased;
            _ = ping_interval.tick() => ping_due = true,
            req = out_rx.recv(), if slot.is_idle() && !closed => match req {
                None => closed = true,
                Some(req) => {
                    let id = take_id(&mut next_id);
                    // Registered before the first network byte, and also when encoding fails part way, so a
                    // FigureGet recorded first still gets its one failure when this connection ends.
                    let mut entries = HashMap::new();
                    let encoded = send_request(&mut slot.bytes, &mut entries, id, req).await;
                    pending.extend(entries);
                    encoded?;
                }
            },
            wrote = slot.advance(&mut tx), if !slot.is_idle() => wrote?,
            done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                // Completed: reclaim the reader and arm the next read.
                let (rx_back, read) = done;
                read_fut = Some(Box::pin(read_owned(rx_back)));
                let (frame, blob) = read?;
                note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                handle_response_frame(frame, blob, pending, evt_tx, &host);
                window.request_redraw();
            }
            () = tokio::time::sleep_until(heard.at() + silence) => {
                if heard.at() + silence <= tokio::time::Instant::now() {
                    return Err(silent(silence));
                }
            }
        }
    }
}

/// The disconnect for a daemon that has sent no byte for `silence`.
fn silent(silence: std::time::Duration) -> anyhow::Error {
    anyhow::anyhow!("nothing from the daemon in {silence:?}")
}

/// `fut`'s output, or the named silence once no byte has come from the daemon for `silence`: the same bound as the
/// steady loop's, for the reads after the outgoing side closed.
async fn within_silence<F: std::future::Future>(
    heard: &LastHeard, silence: std::time::Duration, fut: F,
) -> Result<F::Output> {
    tokio::pin!(fut);
    loop {
        tokio::select! {
            biased;
            out = &mut fut => return Ok(out),
            () = tokio::time::sleep_until(heard.at() + silence) => {
                if heard.at() + silence <= tokio::time::Instant::now() {
                    return Err(silent(silence));
                }
            }
        }
    }
}

/// After the outgoing side closed: handle the read that was in flight, then every frame until the connection ends or
/// the daemon falls silent.
async fn drain_reads<R, Wn>(
    mut rx: R,
    first: Result<(Frame, Option<Vec<u8>>)>,
    pending: &mut HashMap<u64, PendingKind>,
    session: &mut SessionState,
    host: &HostKey,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    window: &Wn,
    heard: &LastHeard,
    silence: std::time::Duration,
) -> Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
    Wn: Redraw,
{
    let mut next = first;
    loop {
        let (frame, blob) = next?;
        note_revision(frame.rev, &mut session.memory, host, &mut session.gate);
        handle_response_frame(frame, blob, pending, evt_tx, host);
        window.request_redraw();
        next = within_silence(heard, silence, codec::read_frame(&mut rx)).await?;
    }
}

#[cfg(test)]
#[path = "steady_tests.rs"]
mod tests;
