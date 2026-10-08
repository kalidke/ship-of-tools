//! The steady control loop: replies and pushed events, the ping, and the window's requests, with one held read and
//! one held write that progress together.

use super::*;

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
pub(super) async fn read_owned<R: AsyncRead + Unpin>(
    mut rx: tokio::io::BufReader<R>,
) -> (tokio::io::BufReader<R>, Result<(Frame, Option<Vec<u8>>)>) {
    let res = codec::read_frame(&mut rx).await;
    (rx, res)
}

/// The steady-state loop: replies and pushed events, the ping, and the window's requests.
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
            let (rx, read) = read_fut.take().expect("read_fut is always Some here").await;
            return drain_reads(rx, read, pending, session, &host, evt_tx, window).await;
        }
        // Poll both held futures; completing either never restarts the other's partial frame.
        tokio::select! {
            done = read_fut.as_mut().expect("read_fut is always Some at loop top") => {
                // Completed: reclaim the reader and arm the next read.
                let (rx_back, read) = done;
                read_fut = Some(Box::pin(read_owned(rx_back)));
                let (frame, blob) = read?;
                note_revision(frame.rev, &mut session.memory, &host, &mut session.gate);
                handle_response_frame(frame, blob, pending, evt_tx, &host);
                window.request_redraw();
            }
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
        }
    }
}

/// After the outgoing side closed: handle the read that was in flight, then every frame until the connection ends.
async fn drain_reads<R, Wn>(
    mut rx: tokio::io::BufReader<R>,
    first: Result<(Frame, Option<Vec<u8>>)>,
    pending: &mut HashMap<u64, PendingKind>,
    session: &mut SessionState,
    host: &HostKey,
    evt_tx: &StdSender<(HostKey, IncomingEvt)>,
    window: &Wn,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    Wn: Redraw,
{
    let mut next = first;
    loop {
        let (frame, blob) = next?;
        note_revision(frame.rev, &mut session.memory, host, &mut session.gate);
        handle_response_frame(frame, blob, pending, evt_tx, host);
        window.request_redraw();
        next = codec::read_frame(&mut rx).await;
    }
}

#[cfg(test)]
#[path = "steady_tests.rs"]
mod tests;
