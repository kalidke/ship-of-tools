//! Voyage verifier (ADR 0039 §Verifier). Implements the full `sot-log
//! verify` checklist: wrapper/CRC validity (via the reader), seal digests +
//! chain, filename⇔header identity, index continuity, epoch monotonicity
//! (nondecreasing, run-boundary changes), per-epoch `n` contiguity,
//! quiescent-state file counts, structural ref resolution,
//! capture-before-inline-input, the cross-field matrix (actor/lifecycle/
//! control_exchange requireds+forbiddens, stream/transformed attached_to,
//! turn_close uniqueness+target), the input_fact chain lattice per
//! idem_key, stream `prev`-chain linearity per (attached_to, cell),
//! take_epoch strict ordering (the null-holder-first-in-epoch rule and
//! controller-actor agreement), and blob presence + length for
//! artifact_ref and payload_ref.
//!
//! Out of scope for this module by design (ADR 0039's "Merge gates for the
//! crate" list, not the `sot-log verify` checklist): cross-language golden
//! fixtures and the crash/fault harness — those exercise the writer and
//! recovery paths, not this reader-side pass.

use crate::store::dedupe::FactObj;
use crate::store::envelope::{
    validate_blob_ref, validate_str128, ActorKind, BlobRef, Class, ExchangePhase, InputContent,
    InputFactKind, LifecycleKind, RefKind, Seq,
};
use crate::store::segment::{SegmentIdentity, SegmentReader, SegmentState};
use crate::store::voyage::BLOBS_DIR;
use crate::{Error, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// `lifecycle.kind=take_state`'s `take` object.
#[derive(Deserialize)]
struct TakeObj {
    take_epoch: u64,
    #[serde(default)]
    holder: Option<String>,
}

/// Per-idem_key WAL state (ADR 0039 "Input WAL + dedupe" lattice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FactState {
    Input,
    Intent,
    Forwarded,
    Observed,
    Refused,
}

/// The three registered required features (ADR 0039 registry, 2026-08-24;
/// third entry 2026-08-30, ADR 0041 step 6 U1b). `json-f64-v1` is enforced
/// bidirectionally (undeclared + fractional = loud, inline or spilled).
/// `cgroup-fence-v1` is likewise bidirectional since the wiring PR fixed
/// the spawn-detail schema: a `producer_spawn` whose `kill_domain` bears
/// authority (scheme "cgroup") must sit in a segment declaring the
/// feature; scheme "none" and an absent `kill_domain` claim no authority;
/// unknown schemes fail closed. `run-end-requested-v1` is bidirectional
/// the same way: a `run_end_requested` frame requires its segment to
/// declare it (below), and a reader built before this constant grew a
/// third entry refuses ANY segment that declares an unknown feature name
/// (the loop just below this one) before it would ever decode the frame
/// — the ADR 0041 "reader lands one release before the writer" property,
/// for free, from the SAME mechanism the two existing entries already
/// use.
pub const REGISTERED_FEATURES: [&str; 3] = [
    "sot.producer.json-f64-v1",
    "sot.capsule.cgroup-fence-v1",
    "sot.capsule.run-end-requested-v1",
];

/// Turn-closure predicate (ADR 0039 §Verifier, ADR 0040).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Zero unmatched turn_opens. The default and the only CERTIFYING mode.
    Complete,
    /// Non-certifying diagnostic: tolerates at most one unmatched open,
    /// and only in the currently open tip's epoch. A verifier cannot prove
    /// a writer is live; only the owning capsule may treat this as health.
    AllowOpenTip,
}

mod frame;
mod leg;
mod lifecycle;
mod pass;

pub use leg::{leg_carries_run_end_marker, leg_producer_uptime_ms};
pub use pass::verify_voyage_mode;

/// Certifying verification (Complete mode).
pub fn verify_voyage(root: &Path, voyage_id: &str) -> Result<()> {
    verify_voyage_mode(root, voyage_id, VerifyMode::Complete)
}

/// Producer payloads without the f64 feature: every number must be an
/// integer with |v| <= 2^53-1 (the §3 atoms), recursively.
fn check_integer_numbers(v: &serde_json::Value) -> std::result::Result<(), String> {
    match v {
        serde_json::Value::Number(n) => {
            let ok = n.as_i64().map(|i| i.unsigned_abs() <= crate::store::envelope::U53_MAX)
                .or_else(|| n.as_u64().map(|u| u <= crate::store::envelope::U53_MAX))
                .unwrap_or(false);
            if ok { Ok(()) } else { Err(format!("non-integer or out-of-range number {n}")) }
        }
        serde_json::Value::Array(a) => a.iter().try_for_each(check_integer_numbers),
        serde_json::Value::Object(m) => m.values().try_for_each(check_integer_numbers),
        _ => Ok(()),
    }
}

/// Read a blob's bytes from the CAS (existence/length already verified by
/// `check_blob_on_disk` in the same pass; this re-reads for content checks).
fn read_blob(root: &Path, blob: &BlobRef, seq: Seq) -> Result<Vec<u8>> {
    let path = root
        .join(BLOBS_DIR)
        .join(&blob.algo)
        .join(&blob.digest[0..2])
        .join(&blob.digest);
    std::fs::read(&path).map_err(|e| Error::Schema(format!(
        "frame {seq:?}: payload_ref blob unreadable: {e}"
    )))
}

/// A `BlobRef` must resolve to a CAS file of exactly the recorded length.
fn check_blob_on_disk(root: &Path, blob: &BlobRef, seq: Seq) -> Result<()> {
    validate_blob_ref(blob)?;
    let path = root
        .join(BLOBS_DIR)
        .join(&blob.algo)
        .join(&blob.digest[0..2])
        .join(&blob.digest);
    let meta = std::fs::metadata(&path).map_err(|_| Error::Corrupt {
        offset: 0,
        what: format!("frame {seq:?}: blob {} missing on disk", blob.digest),
    })?;
    if meta.len() != blob.length {
        return Err(Error::Corrupt {
            offset: 0,
            what: format!(
                "frame {seq:?}: blob {} length {} != recorded {}",
                blob.digest,
                meta.len(),
                blob.length
            ),
        });
    }
    Ok(())
}

/// A `forwarded`/`producer_observed` fact's `intent` ref must name an
/// earlier `forward_intent` fact frame for the SAME idem_key.
fn check_intent_ref(
    seq: Seq,
    fact_kind: InputFactKind,
    intent: Option<Seq>,
    idem_key: &str,
    intent_owner: &HashMap<(u64, u64), String>,
) -> Result<()> {
    let intent = intent
        .ok_or_else(|| Error::Schema(format!("lifecycle {seq:?}: {fact_kind:?} needs intent")))?;
    if intent_owner.get(&(intent.epoch, intent.n)).map(String::as_str) != Some(idem_key) {
        return Err(Error::Schema(format!(
            "lifecycle {seq:?}: intent {intent:?} does not name this input's forward_intent"
        )));
    }
    Ok(())
}


#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
mod support_tests;
#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
mod features_tests;
#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
mod matrix_tests;
