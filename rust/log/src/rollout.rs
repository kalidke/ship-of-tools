//! ADR 0041 "Upgrade and version skew" — the reader-first rollout gate
//! for a feature-bearing segment (ADR 0039 `required_features` registry).
//!
//! Rollout is two-phase: the READER lands one release before the writer
//! (a release that can decode `sot.capsule.run-end-requested-v1` ships
//! first; the release that actually WRITES it ships after). But
//! publication order alone is not the guarantee the ADR pins — release
//! discovery jumps to the newest release, so a machine on a pre-reader
//! release can skip the reader release entirely and land straight on the
//! writer. What actually protects a ROLLBACK is that the writer may not
//! open a feature-bearing segment until the INSTALLED rollback target's
//! reader can decode one — "publication adjacency is not installation
//! history."
//!
//! This module is the LIBRARY-side enforcement point: [`gate`] is the
//! pure decision, taking [`RolloutEvidence`] — a TYPED, identity-bound
//! record (Codex round-1 Major 9 discharge). The original version of
//! this module took `Option<&[String]>` and treated `None` as
//! authorization ("no rollback target to protect") — which silently
//! conflated "a transaction verified there is nothing to protect" with
//! "nobody has ever recorded anything at all", the exact confusion an
//! upgrade from a pre-U4 installation would hit for free. `gate` now
//! takes no `Option` at all: a caller with no real evidence has no
//! [`RolloutEvidence`] value it may honestly construct, and must refuse
//! before ever calling this function — never silently pass a stand-in
//! that reads as authorization.
//!
//! [`read_rollout_evidence`] is a PROVISIONAL read-side primitive, kept
//! for U4 to adapt or replace: the release-apply transaction that WRITES
//! this evidence (ADR 0041 "Upgrade and version skew" step 0's
//! PREFLIGHT: "transaction metadata records the INSTALLED target's
//! reader feature set") is step 6 unit U4's work and does not exist in
//! this crate yet. Its file name and JSON shape are NOT frozen — U4 owns
//! the final shape once the real transaction exists — and, per Major 9,
//! it is NOT wired into any real binary's startup path: `sot-capsule.rs`
//! (a manual testing harness with no installation history to prove
//! anything against) constructs [`RolloutEvidence::NoRollbackTarget`]
//! directly rather than reading this file, so a stopgap read can never
//! quietly become load-bearing on the real binary.

use crate::{Error, Result};
use std::path::Path;

/// Typed, identity-bound evidence for [`gate`]. MISSING evidence must
/// never be silently treated as "no rollback target" — that would let a
/// simply-absent record (every machine before U4 ships) forge the same
/// authorization a genuine first-install verification would. So there
/// are exactly two constructible states, both affirmative claims, and no
/// third "I don't know" value that reads as permission:
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RolloutEvidence {
    /// A transaction has POSITIVELY VERIFIED there is no installed
    /// rollback target (a genuine first-ever install) — never the
    /// default read for "the evidence file doesn't exist yet". A no-op
    /// for `gate`: there is nothing to protect.
    NoRollbackTarget,
    /// An installed rollback target exists, identified by its own
    /// release/build identity — not a bare feature array with no
    /// identity behind it, which a stale record for a DIFFERENT target
    /// could satisfy just as easily as a current one.
    Installed {
        /// The installed target's own release identifier (e.g. a
        /// version string) — diagnostic, and load-bearing evidence that
        /// this is a specific, real target.
        release: String,
        /// What this evidence is a rollback target FOR (e.g. a build
        /// identifier) — lets a future caller detect evidence collected
        /// for a target other than the one asking.
        target: String,
        /// The installed target's reader feature set.
        reader_features: Vec<String>,
    },
}

/// A writer may not open a segment declaring `feature` unless `evidence`
/// affirmatively clears it: [`RolloutEvidence::NoRollbackTarget`]
/// (nothing to protect) or [`RolloutEvidence::Installed`] whose
/// `reader_features` already contains `feature`. There is no third,
/// implicit "unknown means yes" case — a caller with no real evidence
/// must refuse before ever reaching this function (see the module doc).
pub fn gate(evidence: &RolloutEvidence, feature: &str) -> Result<()> {
    match evidence {
        RolloutEvidence::NoRollbackTarget => Ok(()),
        RolloutEvidence::Installed {
            release,
            target,
            reader_features,
        } => {
            if reader_features.iter().any(|f| f == feature) {
                Ok(())
            } else {
                Err(Error::State(format!(
                    "writer may not open a segment declaring {feature:?}: the installed rollback \
                     target ({release} @ {target})'s reader cannot decode it \
                     (ADR 0041 reader-first rollout)"
                )))
            }
        }
    }
}

/// Read a [`RolloutEvidence`] record from
/// `<state_dir>/rollout-evidence.json`. PROVISIONAL (see the module doc)
/// — the name and shape are U4's to finalize. `Ok(None)` for "file
/// absent": callers MUST NOT map that to
/// [`RolloutEvidence::NoRollbackTarget`] themselves (see that variant's
/// own doc); the honest response to "no evidence at all" is to refuse,
/// not to guess. A PRESENT but unparseable file is loud (`Err`), never
/// silently treated as absent.
pub fn read_rollout_evidence(state_dir: &Path) -> Result<Option<RolloutEvidence>> {
    #[derive(serde::Deserialize)]
    #[serde(tag = "state", rename_all = "snake_case")]
    enum Wire {
        NoRollbackTarget,
        Installed {
            release: String,
            target: String,
            reader_features: Vec<String>,
        },
    }
    let path = state_dir.join("rollout-evidence.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let wire: Wire = serde_json::from_slice(&bytes).map_err(|e| {
                Error::Schema(format!(
                    "{}: does not parse as rollout evidence: {e}",
                    path.display()
                ))
            })?;
            Ok(Some(match wire {
                Wire::NoRollbackTarget => RolloutEvidence::NoRollbackTarget,
                Wire::Installed {
                    release,
                    target,
                    reader_features,
                } => RolloutEvidence::Installed {
                    release,
                    target,
                    reader_features,
                },
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(features: &[&str]) -> RolloutEvidence {
        RolloutEvidence::Installed {
            release: "0.5.9".to_string(),
            target: "test-target".to_string(),
            reader_features: features.iter().map(|f| f.to_string()).collect(),
        }
    }

    #[test]
    fn no_rollback_target_is_a_no_op() {
        gate(&RolloutEvidence::NoRollbackTarget, "sot.capsule.run-end-requested-v1").unwrap();
    }

    #[test]
    fn installed_target_supporting_the_feature_passes() {
        gate(&installed(&["sot.capsule.run-end-requested-v1"]), "sot.capsule.run-end-requested-v1")
            .unwrap();
    }

    #[test]
    fn installed_target_missing_the_feature_fails_closed() {
        let err = gate(&installed(&["sot.producer.json-f64-v1"]), "sot.capsule.run-end-requested-v1")
            .unwrap_err();
        assert!(format!("{err}").contains("cannot decode"), "got: {err}");
        assert!(format!("{err}").contains("0.5.9"), "expected the release identity in the error: {err}");
    }

    #[test]
    fn empty_installed_set_fails_closed() {
        assert!(gate(&installed(&[]), "sot.capsule.run-end-requested-v1").is_err());
    }

    #[test]
    fn absent_file_reads_as_none_never_as_no_rollback_target() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_rollout_evidence(dir.path()).unwrap(), None);
    }

    #[test]
    fn present_file_reads_no_rollback_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rollout-evidence.json"),
            br#"{"state":"no_rollback_target"}"#,
        )
        .unwrap();
        assert_eq!(
            read_rollout_evidence(dir.path()).unwrap(),
            Some(RolloutEvidence::NoRollbackTarget)
        );
    }

    #[test]
    fn present_file_reads_installed_evidence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rollout-evidence.json"),
            br#"{"state":"installed","release":"0.5.9","target":"test-target",
                 "reader_features":["sot.capsule.run-end-requested-v1"]}"#,
        )
        .unwrap();
        assert_eq!(
            read_rollout_evidence(dir.path()).unwrap(),
            Some(installed(&["sot.capsule.run-end-requested-v1"]))
        );
    }

    #[test]
    fn corrupt_file_is_loud_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("rollout-evidence.json"), b"not json").unwrap();
        assert!(read_rollout_evidence(dir.path()).is_err());
    }
}
