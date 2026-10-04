//! Mail delivery in the daemon: the hub link that files relayed messages.

pub(crate) mod bus;
pub(crate) mod filer;
pub(crate) mod forward;
pub(crate) mod hub_link;
pub(crate) mod inbox;
pub(crate) mod relay;

/// 0031 B1: the folder's hub names the lock manager its inbox appends go
/// through; every writer appends locally only when it computes the same
/// name. A guest never writes the record.
pub(crate) fn record_at_boot() {
    if let Some(home) = crate::comm::sot_comm_home().filter(|h| h.is_dir()) {
        let self_host = filer::comm_self_host();
        let topology_hub = filer::comm_topology_hub(&sot_protocol::topology::load(), &self_host);
        // inbox/ first: its lock manager is computed for inbox/, as the scripts compute it.
        let inbox = home.join("inbox");
        let started = std::fs::create_dir_all(&inbox).and_then(|()| {
            let id = inbox::lock_identity(&inbox);
            inbox::record_at_start(&home, topology_hub, &id, inbox::machine_id().as_deref()).map(|r| (id, r))
        });
        match started {
            Ok((id, (_, inbox::AtStart::Create))) => tracing::info!(%id, "comm inbox lock manager recorded"),
            Ok((id, (_, inbox::AtStart::Current))) => tracing::info!(%id, "comm inbox lock record is current"),
            Ok((id, (_, inbox::AtStart::Replace))) => tracing::warn!(%id, "comm inbox lock manager re-recorded after a remount"),
            Ok((_, (inbox::Role::Hub, inbox::AtStart::Keep(why)))) => tracing::error!("{why}"),
            Ok((_, (inbox::Role::Guest, inbox::AtStart::Keep(why)))) => tracing::info!("{why}"),
            Err(e) => tracing::warn!(error = %e, "comm inbox lock record not written"),
        }
    }
}
