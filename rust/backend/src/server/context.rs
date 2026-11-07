//! The daemon's services as one value.

use super::*;
use crate::comm::mail::links::Links;
use crate::lifecycle::lease::Leases;
use crate::topology::store::{TopologyChanged, TopologyStore};

/// The daemon's services, built once in `run`. A connection is given this value and each handler is given only what
/// it uses, selected in `dispatch::serve`; a service is added here, not as another parameter down the chain.
#[derive(Clone)]
pub(super) struct Services {
    pub(super) session: Session,
    pub(super) mathjax: MathJax,
    pub(super) pluto: Pluto,
    pub(super) files_mode: Arc<FilesMode>,
    pub(super) preview_changed_tx: broadcast::Sender<PreviewChanged>,
    pub(super) label: Arc<Option<String>>,
    pub(super) workspaces: Workspaces,
    pub(super) ws_events_tx: broadcast::Sender<WorkspaceChanged>,
    pub(super) agent_events_tx: broadcast::Sender<AgentMessage>,
    pub(super) links: Links,
    pub(super) fe_command_tx: broadcast::Sender<FeCommandEvt>,
    pub(super) repl_frame_tx: broadcast::Sender<ReplFrameMsg>,
    pub(super) clients: Clients,
    pub(super) topology_store: Arc<TopologyStore>,
    pub(super) topo_changed_tx: broadcast::Sender<TopologyChanged>,
    pub(super) leases: Arc<Leases>,
    pub(super) chat: Arc<crate::chat::Chat>,
}

#[cfg(test)]
impl Services {
    /// A service set of its own for a test: its own roster, buses and registry, rooted at `root`.
    pub(super) fn for_test(label: &str, root: &std::path::Path) -> Self {
        Services {
            session: Session::new(),
            mathjax: MathJax::new(MathJax::default_script_path(), sot_containment::process()),
            pluto: Pluto::new(Pluto::default_project_dir(), Pluto::default_start_script()),
            files_mode: Arc::new(FilesMode::new(root.to_path_buf()).unwrap()),
            preview_changed_tx: broadcast::channel(8).0,
            label: Arc::new(Some(label.to_string())),
            workspaces: Workspaces::new(),
            ws_events_tx: broadcast::channel(8).0,
            agent_events_tx: broadcast::channel(8).0,
            links: Links::default(),
            fe_command_tx: broadcast::channel(8).0,
            repl_frame_tx: broadcast::channel(8).0,
            clients: Clients::new(),
            topology_store: Arc::new(TopologyStore::new(root.join("hosts.toml"))),
            topo_changed_tx: broadcast::channel(8).0,
            leases: Arc::new(Leases::new(Some("boot".to_string()), None, None, false)),
            chat: crate::chat::Chat::off(),
        }
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
