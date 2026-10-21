use super::*;
use crate::identity::challenge::{PeerAuthOutcome, PeerAuthenticated};

#[test]
fn map_peer_auth_outcome_authenticated_is_ok() {
    let outcome = PeerAuthOutcome::Authenticated(PeerAuthenticated { pid: 4242, created: 7 });
    assert!(map_peer_auth_outcome(outcome).is_ok());
}

#[test]
fn map_peer_auth_outcome_foreign_is_the_typed_transport_error() {
    assert!(matches!(map_peer_auth_outcome(PeerAuthOutcome::Foreign), Err(TransportError::Foreign)));
}

#[test]
fn map_peer_auth_outcome_undetermined_is_the_typed_transport_error() {
    assert!(matches!(
        map_peer_auth_outcome(PeerAuthOutcome::Undetermined),
        Err(TransportError::Undetermined { via: "direct", .. })
    ));
}
