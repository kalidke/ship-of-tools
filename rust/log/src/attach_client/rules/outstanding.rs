//! Ruling (c): an outstanding input survives reconnect within one voyage (`OutstandingSlot`).

// ---------------------------------------------------------------------
// (c) Outstanding input survives reconnect, exactly once, within one voyage
// ---------------------------------------------------------------------

/// The exact tuple the ADR pins: `(voyage_uuid, idem_key, take_epoch,
/// bytes)` — at most one outstanding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingInput {
    pub voyage_uuid: String,
    pub idem_key: [u8; 16],
    pub take_epoch: u64,
    pub bytes: Vec<u8>,
}

/// The wire's three terminal answers to an `input` frame (ADR 0041:
/// "the wire ... defines three terminal answers").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputWireOutcome {
    Recorded,
    DeliveryUnknown,
    RefusedStale,
}

/// What applying an outcome resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutstandingResolution {
    /// `input_recorded`: completes.
    Completed,
    /// `input_delivery_unknown`: "never auto-retried ... dropped and
    /// marked visibly unknown."
    Unknown,
    /// `input_refused_stale`: "re-sent under the new epoch with a NEW
    /// key — the old chain is closed by the refusal."
    RetryNewEpoch { idem_key: [u8; 16] },
    /// Nothing was outstanding — a caller applying a stray reply (should
    /// not happen in a correct driver; kept explicit rather than
    /// panicking so a protocol surprise degrades to a no-op instead of a
    /// crash).
    NothingOutstanding,
}

/// What a reconnect's own re-take should do with whatever was
/// outstanding when the connection dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconnectResendDecision {
    /// "Resends THE SAME KEY" under the freshly re-taken epoch.
    Resend { idem_key: [u8; 16] },
    /// The voyage UUID changed: "CANCELED and marked unknown, never
    /// replayed and never re-keyed." Carries the canceled tuple so the
    /// caller can surface it visibly rather than dropping it silently.
    Cancel { canceled: OutstandingInput },
    /// Nothing was outstanding.
    None,
}

/// At most one outstanding `input`, ADR-tracked across reconnects within
/// one voyage.
#[derive(Debug, Default)]
pub struct OutstandingSlot(Option<OutstandingInput>);

impl OutstandingSlot {
    pub fn new() -> Self {
        Self(None)
    }

    pub fn outstanding(&self) -> Option<&OutstandingInput> {
        self.0.as_ref()
    }

    /// Records a fresh outstanding input (the take-transaction's flush,
    /// or ordinary steady-state typing while DRIVING). `mint_key` is
    /// injected so tests can pin the key; production callers pass a
    /// `getrandom`-backed closure.
    pub fn record(
        &mut self,
        voyage_uuid: String,
        take_epoch: u64,
        bytes: Vec<u8>,
        mint_key: impl FnOnce() -> [u8; 16],
    ) -> [u8; 16] {
        let idem_key = mint_key();
        self.0 = Some(OutstandingInput { voyage_uuid, idem_key, take_epoch, bytes });
        idem_key
    }

    /// Applies one of the wire's three terminal answers to whatever is
    /// currently outstanding.
    pub fn apply_outcome(
        &mut self,
        outcome: InputWireOutcome,
        new_epoch: u64,
        mint_key: impl FnOnce() -> [u8; 16],
    ) -> OutstandingResolution {
        let Some(o) = self.0.as_mut() else {
            return OutstandingResolution::NothingOutstanding;
        };
        match outcome {
            InputWireOutcome::Recorded => {
                self.0 = None;
                OutstandingResolution::Completed
            }
            InputWireOutcome::DeliveryUnknown => {
                self.0 = None;
                OutstandingResolution::Unknown
            }
            InputWireOutcome::RefusedStale => {
                let idem_key = mint_key();
                o.idem_key = idem_key;
                o.take_epoch = new_epoch;
                OutstandingResolution::RetryNewEpoch { idem_key }
            }
        }
    }

    /// After a reconnect re-attaches and re-takes: resend under the SAME
    /// key within the same voyage; cancel across a voyage change.
    pub fn resend_after_reconnect(
        &mut self,
        new_voyage_uuid: &str,
        new_take_epoch: u64,
    ) -> ReconnectResendDecision {
        let Some(o) = self.0.as_mut() else {
            return ReconnectResendDecision::None;
        };
        if o.voyage_uuid != new_voyage_uuid {
            let canceled = self.0.take().expect("checked Some above");
            return ReconnectResendDecision::Cancel { canceled };
        }
        o.take_epoch = new_take_epoch;
        ReconnectResendDecision::Resend { idem_key: o.idem_key }
    }

    /// A quit or cancel with a key outstanding "reports it rather than
    /// dropping it silently" — the caller surfaces the returned value,
    /// never discards it unseen.
    pub fn cancel_for_quit(&mut self) -> Option<OutstandingInput> {
        self.0.take()
    }
}

#[cfg(test)]
#[path = "outstanding_tests.rs"]
mod tests;
