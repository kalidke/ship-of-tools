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
//! pure decision, and [`read_installed_reader_features`] is a real (not
//! stubbed) way to source its input from disk. What is genuinely NOT
//! built here — and is explicitly out of this unit's scope, not a gap in
//! it — is the release-apply TRANSACTION that WRITES the file
//! [`read_installed_reader_features`] reads: ADR 0041 "Upgrade and
//! version skew" step 0's PREFLIGHT says "transaction metadata records
//! the INSTALLED target's reader feature set", and that transaction
//! (the applier's own step-0/1/2/3 sequence) is step 6 unit U4's work,
//! which does not exist in this crate yet. Until U4 wires a real writer
//! for it, the file is simply absent on every machine, which this
//! module treats as "no rollback target to protect" (see `gate`'s doc)
//! — never as a silent bypass, since the check still runs and still
//! fails closed the moment a value IS recorded.

use crate::{Error, Result};
use std::path::Path;

/// A writer may not open a segment declaring `feature` unless the
/// INSTALLED rollback target's reader can decode a segment declaring it.
/// `installed_reader_features` is the reader feature set recorded for
/// the currently installed rollback target (see
/// [`read_installed_reader_features`]); `None` means no release-apply
/// transaction has ever recorded one — a first-ever install, or any
/// machine before U4 ships the transaction that records it — which is a
/// no-op: there is no rollback target to protect yet. `Some(installed)`
/// not containing `feature` fails closed: activating the writer would
/// let a rollback land on a release whose reader cannot even open the
/// segment.
pub fn gate(installed_reader_features: Option<&[String]>, feature: &str) -> Result<()> {
    if let Some(installed) = installed_reader_features {
        if !installed.iter().any(|f| f == feature) {
            return Err(Error::State(format!(
                "writer may not open a segment declaring {feature:?}: the installed rollback \
                 target's reader cannot decode it (ADR 0041 reader-first rollout)"
            )));
        }
    }
    Ok(())
}

/// Read the installed rollback target's reader feature set from
/// `<state_dir>/installed-reader-features.json` (a JSON array of
/// strings) — the file [`gate`]'s doc names as U4's future write side.
/// `Ok(None)` for "file absent", which covers both "no transaction has
/// ever run" and "state dir doesn't exist yet": both mean nothing to
/// gate against. A PRESENT but unparseable file is loud (`Err`), never
/// silently treated as absent — a corrupt record must not quietly act
/// like "no rollback target to protect", which would defeat the one
/// property this file exists to prove.
pub fn read_installed_reader_features(state_dir: &Path) -> Result<Option<Vec<String>>> {
    let path = state_dir.join("installed-reader-features.json");
    match std::fs::read(&path) {
        Ok(bytes) => {
            let features: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| {
                Error::Schema(format!(
                    "{}: does not parse as a JSON array of strings: {e}",
                    path.display()
                ))
            })?;
            Ok(Some(features))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_installed_target_is_a_no_op() {
        gate(None, "sot.capsule.run-end-requested-v1").unwrap();
    }

    #[test]
    fn installed_target_supporting_the_feature_passes() {
        let installed = vec!["sot.capsule.run-end-requested-v1".to_string()];
        gate(Some(&installed), "sot.capsule.run-end-requested-v1").unwrap();
    }

    #[test]
    fn installed_target_missing_the_feature_fails_closed() {
        let installed = vec!["sot.producer.json-f64-v1".to_string()];
        let err = gate(Some(&installed), "sot.capsule.run-end-requested-v1").unwrap_err();
        assert!(format!("{err}").contains("cannot decode"), "got: {err}");
    }

    #[test]
    fn empty_installed_set_fails_closed() {
        let installed: Vec<String> = vec![];
        assert!(gate(Some(&installed), "sot.capsule.run-end-requested-v1").is_err());
    }

    #[test]
    fn absent_file_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_installed_reader_features(dir.path()).unwrap(), None);
    }

    #[test]
    fn present_file_reads_the_recorded_set() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("installed-reader-features.json"),
            br#"["sot.capsule.run-end-requested-v1"]"#,
        )
        .unwrap();
        assert_eq!(
            read_installed_reader_features(dir.path()).unwrap(),
            Some(vec!["sot.capsule.run-end-requested-v1".to_string()])
        );
    }

    #[test]
    fn corrupt_file_is_loud_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("installed-reader-features.json"), b"not json").unwrap();
        assert!(read_installed_reader_features(dir.path()).is_err());
    }
}
