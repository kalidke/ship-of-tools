//! What a state folder keeps for its supervisor: the one-authority fence (`supervisor.lock`) and the voyage pointer
//! (`drawer.voyage`). There is no operation journal: an operation is answered while its supervisor lives, and nothing
//! outlives the supervisor to reconcile.

pub mod fence;
pub mod pointer;
