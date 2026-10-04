//! Shared test doubles: a do-nothing client, peer identity and endpoint.

use crate::challenge::{ChallengeOutcome, PeerAuthOutcome};
use crate::client::{Client, Endpoint};
use std::sync::Mutex;
use std::time::Instant;

use crate::client::PeerIdentity;

// -----------------------------------------------------------------
// ADR 0045 decision 6: the health probe uses the voyage id the
// supervisor last reported to THIS client, never a pointer file.
// -----------------------------------------------------------------

/// A do-nothing [`Client`] — [`TestEndpoint::connect_voyage_unchallenged`]
/// is the only method this test ever exercises, and it never touches
/// the connection it returns.
pub(super) struct TestClient;
impl Client for TestClient {
    fn write_all(&self, _bytes: &[u8]) -> Result<(), crate::transport::TransportError> {
        Ok(())
    }
    fn read(&self, _buf: &mut [u8]) -> Result<usize, crate::transport::TransportError> {
        Ok(0)
    }
    fn cancel(&self) {}
}

/// A do-nothing [`PeerIdentity`] — never actually produced by this
/// test's [`TestEndpoint`] (its `challenge`/`authenticate_server` are
/// unreachable stubs), but [`Endpoint::Process`] still needs a
/// concrete type to name.
pub(super) struct TestProcess;
impl PeerIdentity for TestProcess {
    fn pid(&self) -> u32 {
        0
    }
    fn created(&self) -> u64 {
        0
    }
}

/// Records the row/id [`Endpoint::connect_voyage_unchallenged`] was
/// asked for and always answers as if the voyage pipe were reachable
/// (`Ok`) — proving [`on_supervisor_absent_or_unresponsive`] probes
/// the SAME row and id its caller passed in, never a pointer file it
/// reads itself and never the voyage id in BOTH slots (ADR 0045 lane
/// B4a Codex review blocker: the probe used to send `(voyage,
/// voyage)`, which a real `DaemonLaneEndpoint` reads as "row =
/// voyage id", never resolving to any real row). `connect_
/// supervisor_unchallenged`/`challenge`/`authenticate_server` are
/// unreachable: this test never drives the supervisor lane.
pub(super) struct TestEndpoint {
    pub(super) last_lane_probed: Mutex<Option<String>>,
    pub(super) last_voyage_probed: Mutex<Option<String>>,
}
impl Endpoint for TestEndpoint {
    type Client = TestClient;
    type Process = TestProcess;

    fn connect_voyage_unchallenged(
        &self,
        lane: &str,
        voyage_id: &str,
    ) -> Result<Self::Client, crate::transport::TransportError> {
        *self.last_lane_probed.lock().unwrap() = Some(lane.to_string());
        *self.last_voyage_probed.lock().unwrap() = Some(voyage_id.to_string());
        Ok(TestClient)
    }

    fn connect_supervisor_unchallenged(&self, _lane: &str) -> Result<Self::Client, crate::transport::TransportError> {
        unreachable!("health_probe_uses_the_last_reported_voyage_id never drives the supervisor lane")
    }

    fn challenge(
        &self,
        _conn: &Self::Client,
        _exchange: &mut dyn crate::exchange::IdentityExchange,
        _deadline: Instant,
    ) -> ChallengeOutcome<Self::Process> {
        unreachable!("health_probe_uses_the_last_reported_voyage_id never challenges")
    }

    fn authenticate_server(&self, _conn: &Self::Client) -> PeerAuthOutcome {
        unreachable!("health_probe_uses_the_last_reported_voyage_id never authenticates")
    }
}
