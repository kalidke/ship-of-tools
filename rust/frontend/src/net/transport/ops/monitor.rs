//! monitor.subscribe, monitor.unsubscribe, monitor.history: the requests (send_<op>: write the frame, then record its PendingKind).

use super::*;

pub(crate) async fn send_monitor_subscribe<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
) -> Result<()> {
    tracing::debug!(id, "→ monitor.subscribe");
    codec::write_frame(
        &mut tx,
        &Frame::req(id, op::MONITOR_SUBSCRIBE, serde_json::json!({})),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::MonitorSubscribe);
    Ok(())
}

pub(crate) async fn send_monitor_unsubscribe<W: AsyncWrite + Unpin>(
    mut tx: W,
    id: u64,
) -> Result<()> {
    tracing::debug!(id, "→ monitor.unsubscribe");
    codec::write_frame(
        &mut tx,
        &Frame::req(id, op::MONITOR_UNSUBSCRIBE, serde_json::json!({})),
        None,
    )
    .await?;
    // Fire-and-forget: the backend acks with a bare `{}` we
    // don't track, so there's no pending entry.
    Ok(())
}

pub(crate) async fn send_monitor_history<W: AsyncWrite + Unpin>(
    mut tx: W,
    pending: &mut HashMap<u64, PendingKind>,
    id: u64,
    window_s: f64,
    points: u32,
    until: Option<f64>,
    host: Option<String>,
) -> Result<()> {
    tracing::debug!(window_s, points, ?until, ?host, id, "→ monitor.history");
    codec::write_frame(
        &mut tx,
        &Frame::req(
            id,
            op::MONITOR_HISTORY,
            serde_json::to_value(MonitorHistoryReq {
                window_s,
                until,
                points,
                host,
            })?,
        ),
        None,
    )
    .await?;
    pending.insert(id, PendingKind::MonitorHistory);
    Ok(())
}
