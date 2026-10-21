use super::*;
use sot_log::lane::transport::TransportError;

fn io_transport(kind: std::io::ErrorKind) -> sot_log::Error {
    sot_log::Error::Transport(TransportError::Io {
        op: "connect",
        source: std::io::Error::new(kind, "test"),
    })
}

#[test]
fn no_listener_at_all_is_orphaned() {
    // Decision 27's own "absent" shapes -- what `query_status`'s
    // connect step actually returns on the first attempt when
    // nothing is bound at this row's lane address at all.
    assert!(is_definitely_orphaned(&io_transport(std::io::ErrorKind::NotFound)));
    assert!(is_definitely_orphaned(&io_transport(std::io::ErrorKind::ConnectionRefused)));
}

#[test]
fn a_reachable_but_refusing_lane_is_never_orphaned() {
    // The lane DID answer (a foreign build, a version skew, or
    // some other live-but-unresponsive shape) -- none of these
    // may ever be folded into "nothing is running here".
    assert!(!is_definitely_orphaned(&sot_log::Error::VersionSkew));
    assert!(!is_definitely_orphaned(&sot_log::Error::State(
        "supervisor lane challenge: foreign".to_string()
    )));
}

#[test]
fn a_busy_or_timed_out_endpoint_is_never_orphaned() {
    // Decision 27: a busy endpoint is retried WITHIN the connect
    // bound and only surfaces as `Err` once genuinely ambiguous
    // (e.g. a timeout) -- that must stay unproven, never treated
    // as absent.
    assert!(!is_definitely_orphaned(&io_transport(std::io::ErrorKind::TimedOut)));
    assert!(!is_definitely_orphaned(&io_transport(std::io::ErrorKind::PermissionDenied)));
}
