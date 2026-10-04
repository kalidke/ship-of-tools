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
mod tests {
    use super::*;

    fn key(b: u8) -> [u8; 16] {
        [b; 16]
    }

    // ---- (c) OutstandingSlot -------------------------------------------

    #[test]
    fn record_then_recorded_completes() {
        let mut slot = OutstandingSlot::new();
        let k = slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        assert_eq!(k, key(1));
        assert!(slot.outstanding().is_some());
        let res = slot.apply_outcome(InputWireOutcome::Recorded, 1, || key(2));
        assert_eq!(res, OutstandingResolution::Completed);
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn delivery_unknown_is_never_auto_retried() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let res = slot.apply_outcome(InputWireOutcome::DeliveryUnknown, 1, || key(2));
        assert_eq!(res, OutstandingResolution::Unknown);
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn refused_stale_mints_a_new_key_under_the_new_epoch() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let res = slot.apply_outcome(InputWireOutcome::RefusedStale, 7, || key(2));
        assert_eq!(res, OutstandingResolution::RetryNewEpoch { idem_key: key(2) });
        let o = slot.outstanding().unwrap();
        assert_eq!(o.idem_key, key(2));
        assert_eq!(o.take_epoch, 7);
    }

    #[test]
    fn reconnect_within_the_same_voyage_resends_the_same_key() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let decision = slot.resend_after_reconnect("v1", 9);
        assert_eq!(decision, ReconnectResendDecision::Resend { idem_key: key(1) });
        assert_eq!(slot.outstanding().unwrap().take_epoch, 9);
        assert_eq!(slot.outstanding().unwrap().idem_key, key(1));
    }

    #[test]
    fn reconnect_across_a_voyage_change_cancels_and_reports_the_canceled_tuple() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let decision = slot.resend_after_reconnect("v2", 1);
        match decision {
            ReconnectResendDecision::Cancel { canceled } => {
                assert_eq!(canceled.voyage_uuid, "v1");
                assert_eq!(canceled.bytes, b"hi");
            }
            other => panic!("expected Cancel, got {other:?}"),
        }
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn quit_with_outstanding_reports_it_rather_than_dropping_silently() {
        let mut slot = OutstandingSlot::new();
        slot.record("v1".into(), 1, b"hi".to_vec(), || key(1));
        let reported = slot.cancel_for_quit();
        assert_eq!(reported.unwrap().bytes, b"hi");
        assert!(slot.outstanding().is_none());
    }

    #[test]
    fn apply_outcome_with_nothing_outstanding_is_a_harmless_no_op() {
        let mut slot = OutstandingSlot::new();
        let res = slot.apply_outcome(InputWireOutcome::Recorded, 1, || key(1));
        assert_eq!(res, OutstandingResolution::NothingOutstanding);
    }
}
