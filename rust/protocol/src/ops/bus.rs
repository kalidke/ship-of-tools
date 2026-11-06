// the status frame for events a connection lost to a lagging bus

use super::*;

/// Payload of a `bus.lost` evt. `bus` is the op the lost events travel as (`preview.changed`, `monitor.tick`, ...);
/// `skipped` is how many the receiver reports dropped, 0 if a writer cannot say.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusLostEvt {
    pub bus: String,
    #[serde(default)]
    pub skipped: u64,
}

#[cfg(test)]
#[path = "bus_tests.rs"]
mod tests;
