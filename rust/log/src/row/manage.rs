//! What a row hands the daemon's management calls to. The supervisor implements it; the row names nothing of the
//! supervisor, and a row with none answers the four ops `unknown_op`.

use super::proto::{EndRunRes, ResetRes, StatusRes, StopRes};
use super::run::ConnId;

/// Each call runs in the calling connection's own reader thread and may wait, within the bound its op states.
pub trait Management: Send + Sync {
    /// At once.
    fn status(&self) -> StatusRes;
    /// Ends the run `run` names and waits for its end.
    fn end_run(&self, run: &str, reason: &str) -> EndRunRes;
    /// Starts a new voyage from a resting supervisor and waits for it to be published.
    fn reset(&self) -> ResetRes;
    /// Asks the supervisor to end its agent and exit once `conn`'s reply has been delivered; at once.
    fn stop(&self, conn: ConnId) -> StopRes;
    /// A connection's threads have both ended: its last reply was delivered or given up on.
    fn conn_gone(&self, conn: ConnId);
}
