use super::{protocol_gate, ProtocolGate};

#[test]
fn accepts_matching_protocol() {
    // The backend's own PROTOCOL_VERSION always matches itself.
    assert_eq!(
        protocol_gate(sot_protocol::PROTOCOL_VERSION),
        ProtocolGate::Accept
    );
    // Concretely, protocol 3 is accepted today.
    assert_eq!(protocol_gate(3), ProtocolGate::Accept);
}

#[test]
fn rejects_mismatched_protocol() {
    // The previous protocol (a frontend box that has not converged),
    // a pre-versioning peer (0: the v1 grace ended with the bump to
    // 2) and a newer one are all rejected — the FE renders the
    // "update needed" screen naming both sides.
    assert_eq!(protocol_gate(sot_protocol::PROTOCOL_VERSION - 1), ProtocolGate::Reject);
    // Every pre-0.6.6 client (frontend, hub link, comm scripts) speaks 2: it declares no OS account.
    assert_eq!(protocol_gate(2), ProtocolGate::Reject);
    assert_eq!(protocol_gate(0), ProtocolGate::Reject);
    assert_eq!(protocol_gate(99), ProtocolGate::Reject);
}
