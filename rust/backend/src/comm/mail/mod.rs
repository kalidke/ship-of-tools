//! Mail delivery in the daemon: the hub link that files relayed messages.

pub(crate) mod bus;
pub(crate) mod filer;
pub(crate) mod forward;
pub(crate) mod hub_link;
pub(crate) mod inbox;
pub(crate) mod relay;

use self::inbox as comm_inbox;
use crate::{handlers, paths};

/// 0031 B1: the folder's hub names the lock manager its inbox appends go
/// through; every writer appends locally only when it computes the same
/// name. A guest never writes the record.
pub(crate) fn record_at_boot() {
    if let Some(home) = paths::sot_comm_home().filter(|h| h.is_dir()) {
        let self_host = handlers::comm_self_host();
        let topology_hub = handlers::comm_topology_hub(&sot_protocol::topology::load(), &self_host);
        // inbox/ first: its lock manager is computed for inbox/, as the scripts compute it.
        let inbox = home.join("inbox");
        let started = std::fs::create_dir_all(&inbox).and_then(|()| {
            let id = comm_inbox::lock_identity(&inbox);
            comm_inbox::record_at_start(&home, topology_hub, &id, comm_inbox::machine_id().as_deref()).map(|r| (id, r))
        });
        match started {
            Ok((id, (_, comm_inbox::AtStart::Create))) => tracing::info!(%id, "comm inbox lock manager recorded"),
            Ok((id, (_, comm_inbox::AtStart::Current))) => tracing::info!(%id, "comm inbox lock record is current"),
            Ok((id, (_, comm_inbox::AtStart::Replace))) => tracing::warn!(%id, "comm inbox lock manager re-recorded after a remount"),
            Ok((_, (comm_inbox::Role::Hub, comm_inbox::AtStart::Keep(why)))) => tracing::error!("{why}"),
            Ok((_, (comm_inbox::Role::Guest, comm_inbox::AtStart::Keep(why)))) => tracing::info!("{why}"),
            Err(e) => tracing::warn!(error = %e, "comm inbox lock record not written"),
        }
    }
}
