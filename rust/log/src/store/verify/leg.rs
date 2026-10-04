//! The per-leg readers the supervisor asks: the run-end marker and the producer uptime of one epoch.

use super::*;

/// The READ half of ADR 0041's "Respawn is gated by the typed marker,
/// read from the LATEST LEG AFTER RECONCILIATION" — a small typed
/// accessor for "does this leg's own epoch carry the `run_end_requested`
/// marker", exposed so a later unit's respawn decision has something to
/// call rather than groping the JSON itself. The DECISION (which leg is
/// "latest", what to do about it) is that later unit's job; this
/// function only answers the one question about ONE already-selected
/// epoch.
///
/// Scans every segment file under `seg_dir` whose header names `epoch`
/// exactly, in `.open` or `.sotseg` state — a hard-killed leg's tail
/// segment can remain `.open` and the marker still governs it, per the
/// ADR ("a marker governs only its OWN epoch"). `.recovering`/
/// `.recovering-out` are deliberately NOT read here: those are
/// mid-transaction scratch states reconciliation resolves before this
/// leg's epoch is stable enough to answer "latest" about, matching the
/// ADR's own "after reconciliation" qualifier. Not a certifying pass —
/// no chain, no full cross-field walk; but (Codex round-1 Major 8) it is
/// NOT a bare string grope either: filename identity is cross-checked
/// against the header's OWN claim (the same rule `verify_voyage_mode`
/// enforces), `kind` is decoded through the TYPED closed enum (an
/// unrecognized value is not silently "not a marker" the way a raw
/// string compare would treat it — it is simply not this kind, exactly
/// as the typed decode says), and a CANDIDATE marker frame is verified
/// feature-declared with a well-formed, bounded `reason` before it
/// counts. Every one of those failure shapes — mismatched identity, an
/// authority-changing frame in an undeclaring segment, a malformed
/// reason, or two markers in one epoch — errs LOUD rather than
/// returning `false`: a filename naming epoch E whose header disagrees,
/// or a marker that fails its own shape, must never be silently treated
/// as "no marker", which is exactly the failure mode that could suppress
/// U2's respawn on a genuinely broken record instead of stopping for an
/// operator. A torn or corrupt segment (of any other kind) simply errs
/// too, since only the caller's own already-reconciled leg is ever
/// handed to this function.
pub fn leg_carries_run_end_marker(seg_dir: &Path, voyage_id: &str, epoch: u64) -> Result<bool> {
    let mut found: Option<Seq> = None;
    for entry in std::fs::read_dir(seg_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".tmp" {
            continue;
        }
        let Some((idx, seg_epoch, state)) = SegmentIdentity::parse_file_name(name) else {
            continue;
        };
        if seg_epoch != epoch || !matches!(state, SegmentState::Open | SegmentState::Sealed) {
            continue;
        }
        let id = SegmentIdentity {
            voyage_id: voyage_id.to_string(),
            segment_index: idx,
            epoch: seg_epoch,
        };
        let sealed = state == SegmentState::Sealed;
        let reader = SegmentReader::read(&id.path(seg_dir, state), sealed)?;
        if reader.header.segment_index != idx
            || reader.header.epoch != seg_epoch
            || reader.header.voyage_id != voyage_id
        {
            return Err(Error::State(format!(
                "segment {name}: filename and header identity disagree"
            )));
        }
        let feature_ok = reader
            .header
            .required_features
            .iter()
            .any(|f| f == "sot.capsule.run-end-requested-v1");
        for env in &reader.frames {
            if env.class != Class::Lifecycle {
                continue;
            }
            // Codex round-2b Blocker 3 ("the accessor still silently
            // accepts invalid paths"): a missing/unknown lifecycle kind
            // is a HARD SCHEMA ERROR here, exactly as the full verifier
            // treats it -- never a `continue` that lets a corrupt frame
            // slide past as merely "not this kind". `payload` itself is
            // like`Envelope::validate()`'s own payload/payload_ref XOR
            // (already enforced by `SegmentReader::read`, which runs
            // `env.validate()` on every frame) already guarantees a
            // Lifecycle-class frame carries an inline `payload` --
            // checked again here defensively, still loud if it somehow
            // didn't.
            let payload = env.payload.as_ref().ok_or_else(|| {
                Error::State(format!("lifecycle {:?}: missing payload", env.seq))
            })?;
            let kind: LifecycleKind = payload
                .get("kind")
                .and_then(|k| serde_json::from_value::<LifecycleKind>(k.clone()).ok())
                .ok_or_else(|| {
                    Error::State(format!("lifecycle {:?}: invalid/missing kind", env.seq))
                })?;
            if kind != LifecycleKind::RunEndRequested {
                continue;
            }
            // A marker frame carrying the take/fact fields the cross-
            // field matrix forbids for run_end_requested is corrupt in
            // exactly the way the full verifier refuses -- must not be
            // silently counted as a valid marker.
            if payload.get("take").is_some() || payload.get("fact").is_some() {
                return Err(Error::State(format!(
                    "lifecycle {:?}: run_end_requested forbids take and fact",
                    env.seq
                )));
            }
            if !feature_ok {
                return Err(Error::State(format!(
                    "lifecycle {:?}: run_end_requested in a segment that does not declare \
                     sot.capsule.run-end-requested-v1",
                    env.seq
                )));
            }
            let reason = payload.get("reason").and_then(|r| r.as_str()).ok_or_else(|| {
                Error::State(format!(
                    "lifecycle {:?}: run_end_requested missing a string reason",
                    env.seq
                ))
            })?;
            validate_str128(reason, "run_end_requested.reason")
                .map_err(|e| Error::State(format!("lifecycle {:?}: {e}", env.seq)))?;
            if let Some(prior) = found {
                return Err(Error::State(format!(
                    "epoch {epoch} carries two run_end_requested markers ({prior:?} and {:?})",
                    env.seq
                )));
            }
            found = Some(env.seq);
        }
    }
    Ok(found.is_some())
}

/// The READ half of Codex review round 3, N1: "stability must be judged
/// on the PRODUCER's lifetime, never on the capsule process's exit."
/// Scans `seg_dir` for `epoch`'s own `producer_dead` lifecycle frame and
/// returns its `detail.producer_uptime_ms`, an ADDITIVE free-form
/// diagnostic field (like `detail.reason` already is — no registered
/// feature required, no authority changes). Fail-safe direction is
/// `Ok(None)` for every case that must NOT be trusted as a proven
/// stable duration: no `producer_dead` frame found on this epoch at all
/// (a still-open/unsealed leg, or a spawn-failed leg that never reached
/// a real producer), the key absent from an otherwise well-formed
/// frame, or the value present but not a plain non-negative integer.
/// `None` here is the caller's own cue to count the leg UNSTABLE (N1's
/// own ruling) — never to fall back to a wall-clock measurement a slow
/// teardown could inflate arbitrarily, which is the exact bug this
/// exists to close.
///
/// A STRUCTURALLY corrupt segment (mismatched filename/header identity,
/// a malformed lifecycle envelope) still errs loud here, exactly as
/// [`leg_carries_run_end_marker`] does — this accessor is lenient only
/// about the specific diagnostic VALUE it is looking for, never about
/// the store's own integrity; only an already-reconciled leg is ever
/// handed to it.
pub fn leg_producer_uptime_ms(seg_dir: &Path, voyage_id: &str, epoch: u64) -> Result<Option<u64>> {
    for entry in std::fs::read_dir(seg_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".tmp" {
            continue;
        }
        let Some((idx, seg_epoch, state)) = SegmentIdentity::parse_file_name(name) else {
            continue;
        };
        if seg_epoch != epoch || !matches!(state, SegmentState::Open | SegmentState::Sealed) {
            continue;
        }
        let id = SegmentIdentity {
            voyage_id: voyage_id.to_string(),
            segment_index: idx,
            epoch: seg_epoch,
        };
        let sealed = state == SegmentState::Sealed;
        let reader = SegmentReader::read(&id.path(seg_dir, state), sealed)?;
        if reader.header.segment_index != idx
            || reader.header.epoch != seg_epoch
            || reader.header.voyage_id != voyage_id
        {
            return Err(Error::State(format!(
                "segment {name}: filename and header identity disagree"
            )));
        }
        for env in &reader.frames {
            if env.class != Class::Lifecycle {
                continue;
            }
            let payload = env.payload.as_ref().ok_or_else(|| {
                Error::State(format!("lifecycle {:?}: missing payload", env.seq))
            })?;
            let kind: LifecycleKind = payload
                .get("kind")
                .and_then(|k| serde_json::from_value::<LifecycleKind>(k.clone()).ok())
                .ok_or_else(|| {
                    Error::State(format!("lifecycle {:?}: invalid/missing kind", env.seq))
                })?;
            if kind != LifecycleKind::ProducerDead {
                continue;
            }
            return Ok(payload
                .get("detail")
                .and_then(|d| d.get("producer_uptime_ms"))
                .and_then(serde_json::Value::as_u64));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[cfg(any(target_os = "linux", windows))]
mod tests {
    use super::super::support_tests::store;
    use super::*;
    use crate::envelope::*;
    use crate::segment::{tests::test_env, Commit};
    use serde_json::json;

    /// `leg_carries_run_end_marker` — the small typed accessor a later
    /// unit's respawn decision reads. Must see the marker on a leg's OWN
    /// epoch whether the segment is still `.open` (a hard-killed leg's
    /// tail) or `.sotseg`, and must not see it on a different epoch or an
    /// ordinary lifecycle frame.
    #[test]
    fn leg_carries_run_end_marker_reads_open_and_sealed_legs() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk1");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk1").join("seg");
        // Still .open (never sealed yet): the marker must already be
        // readable — a hard-killed leg's tail segment stays .open.
        assert!(leg_carries_run_end_marker(&seg_dir, "mk1", 1).unwrap());
        assert!(!leg_carries_run_end_marker(&seg_dir, "mk1", 2).unwrap());
        w.seal(None).unwrap();
        assert!(leg_carries_run_end_marker(&seg_dir, "mk1", 1).unwrap());
    }

    /// Codex round-1 Major 8: a marker frame present in a segment that
    /// does NOT declare the feature must fail LOUD (`Err`), never
    /// silently `false` -- an authority-changing frame smuggled past its
    /// own registry rule is corrupt, not "no marker".
    #[test]
    fn leg_carries_run_end_marker_errs_on_undeclared_feature() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk3");
        let mut w = s.open_segment(0).unwrap(); // no features declared
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk3").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk3", 1).unwrap_err();
        assert!(format!("{err}").contains("does not declare"), "got: {err}");
    }

    /// Codex round-1 Major 8: a malformed reason (missing, or over the
    /// str128 bound) on an otherwise feature-declared marker also errs
    /// loud rather than silently reporting "no marker".
    #[test]
    fn leg_carries_run_end_marker_errs_on_malformed_reason() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk4");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "a".repeat(200)}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk4").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk4", 1).unwrap_err();
        assert!(format!("{err}").contains("str128"), "got: {err}");
    }

    /// Codex round-1 Major 8: two well-formed markers in the SAME epoch
    /// (the writer's own first-commit-wins latch is a promise about ITS
    /// process lifetime, not a proof a crafted/corrupted voyage can't
    /// carry two) must err loud, matching the main verifier's own
    /// per-epoch uniqueness rule (Major 7) rather than reporting the
    /// FIRST one found and silently ignoring the second.
    #[test]
    fn leg_carries_run_end_marker_errs_on_duplicate_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk5");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e1 = test_env(1, 1);
        e1.class = Class::Lifecycle;
        e1.payload = Some(json!({"kind": "run_end_requested", "reason": "first"}));
        w.append(&e1, Commit::Immediate).unwrap();
        let mut e2 = test_env(1, 2);
        e2.class = Class::Lifecycle;
        e2.payload = Some(json!({"kind": "run_end_requested", "reason": "second"}));
        w.append(&e2, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk5").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk5", 1).unwrap_err();
        assert!(format!("{err}").contains("carries two"), "got: {err}");
    }

    /// Codex round-1 Major 8: the FILENAME'S epoch is not trusted on its
    /// own — a segment renamed to claim a DIFFERENT epoch than its own
    /// header still carries must err loud rather than silently answering
    /// against the wrong epoch's identity.
    #[test]
    fn leg_carries_run_end_marker_errs_on_filename_header_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk6");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "run_end_requested", "reason": "quit"}));
        w.append(&e, Commit::Immediate).unwrap();
        drop(w);
        let seg_dir = dir.path().join("mk6").join("seg");
        // The real (unsealed) file is named for epoch 1; rename it to
        // CLAIM epoch 2 on disk while its header still says 1.
        let real_name = std::fs::read_dir(&seg_dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name();
        let renamed = SegmentIdentity {
            voyage_id: "mk6".to_string(),
            segment_index: 0,
            epoch: 2,
        }
        .path(&seg_dir, SegmentState::Open);
        std::fs::rename(seg_dir.join(&real_name), &renamed).unwrap();
        let err = leg_carries_run_end_marker(&seg_dir, "mk6", 2).unwrap_err();
        assert!(format!("{err}").contains("identity disagree"), "got: {err}");
    }

    /// Codex round-2b Blocker 3: a missing/unknown lifecycle `kind` on
    /// ANY lifecycle frame in the scanned epoch must err loud -- exactly
    /// as the full verifier treats it -- never silently `continue` as
    /// "not this kind" the way a bare-string-compare accessor would.
    #[test]
    fn leg_carries_run_end_marker_errs_on_invalid_lifecycle_kind() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk7");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "not_a_real_lifecycle_kind"}));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk7").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk7", 1).unwrap_err();
        assert!(format!("{err}").contains("invalid/missing kind"), "got: {err}");
    }

    /// Codex round-2b Blocker 3: a `run_end_requested` frame that ALSO
    /// carries `take` or `fact` (forbidden by the cross-field matrix)
    /// must err loud rather than being counted as a valid marker.
    #[test]
    fn leg_carries_run_end_marker_errs_on_marker_with_forbidden_take() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk8");
        let mut w = s
            .open_segment_with_features(0, vec!["sot.capsule.run-end-requested-v1".to_string()])
            .unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({
            "kind": "run_end_requested",
            "reason": "quit",
            "take": {"take_epoch": 1, "holder": null}
        }));
        w.append(&e, Commit::Immediate).unwrap();
        let seg_dir = dir.path().join("mk8").join("seg");
        let err = leg_carries_run_end_marker(&seg_dir, "mk8", 1).unwrap_err();
        assert!(format!("{err}").contains("forbids take and fact"), "got: {err}");
    }

    #[test]
    fn leg_without_the_marker_reads_false() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(dir.path(), "mk2");
        let mut w = s.open_segment(0).unwrap();
        let mut e = test_env(1, 1);
        e.class = Class::Lifecycle;
        e.payload = Some(json!({"kind": "producer_dead", "detail": {"exit_code": 0}}));
        w.append(&e, Commit::Immediate).unwrap();
        w.seal(None).unwrap();
        let seg_dir = dir.path().join("mk2").join("seg");
        assert!(!leg_carries_run_end_marker(&seg_dir, "mk2", 1).unwrap());
    }
}
