//! Reset: the pointer rename-aside and the recovery of a crashed reset, as journaled.

use crate::supervisor::*;

// ---------------------------------------------------------------------
// Reset
// ---------------------------------------------------------------------

/// Rename the current pointer aside (evidence-preserving, no-replace) if
/// one exists, then mint `new_voyage` fresh. `aside_name` is the exact
/// filename to rename it to: `Some(name)` for the live, journaled path
/// (chosen and recorded AT ADMISSION time); `None` for the no-supervisor
/// CLI path, which journals nothing and so mints its own name here.
pub(in crate::supervisor) fn reset_pointer(state_dir: &Path, new_voyage: &str, aside_name: Option<&str>) -> crate::Result<()> {
    let live = pointer::pointer_path(state_dir);
    if live.exists() {
        crate::fsutil::fsync_file(&live).map_err(|e| {
            err_state(format!("reset_pointer: flushing the live pointer {live:?} before renaming it aside: {e}"))
        })?;
        let owned;
        let name: &str = match aside_name {
            Some(n) => n,
            None => {
                owned = mint_aside_name()?;
                &owned
            }
        };
        let aside = state_dir.join(name);
        crate::fsutil::publish_noreplace(&live, &aside).map_err(|e| {
            err_state(format!("reset_pointer: renaming {live:?} aside to {aside:?}: {e}"))
        })?;
    }
    std::fs::create_dir_all(voyages_dir(state_dir))
        .map_err(|e| err_state(format!("reset_pointer: creating {:?}: {e}", voyages_dir(state_dir))))?;
    let root = voyage_root_path(state_dir, new_voyage);
    if !root.exists() {
        VoyageStore::bootstrap(&root, new_voyage, RetentionClass::Archive)
            .map_err(|e| err_state(format!("reset_pointer: bootstrapping {root:?}: {e}")))?;
    }
    pointer::publish(state_dir, new_voyage)
        .map_err(|e| err_state(format!("reset_pointer: publishing the new pointer for {new_voyage:?}: {e}")))?;
    Ok(())
}

pub(in crate::supervisor) fn reconcile_reset(
    state_dir: &Path,
    op_id: &str,
    new_voyage: &str,
    old_voyage: Option<&str>,
    aside: Option<&str>,
) -> crate::Result<()> {
    match pointer::validate(state_dir) {
        PointerState::Valid(id) if id == new_voyage => {}
        PointerState::Valid(id) if Some(id.as_str()) == old_voyage => {
            reset_pointer(state_dir, new_voyage, aside)?;
        }
        PointerState::NotFound => match (old_voyage, aside) {
            (Some(_), Some(aside_name)) => {
                if !state_dir.join(aside_name).exists() {
                    return Err(err_state(format!(
                        "reset {op_id}: the pointer is absent but its recorded evidence rename \
                         {aside_name:?} does not exist — an operator must investigate before this \
                         can be resumed"
                    )));
                }
                reset_pointer(state_dir, new_voyage, aside)?;
            }
            (Some(_), None) => {
                return Err(err_state(format!(
                    "reset {op_id}: a pointer existed at admission but no aside filename was \
                     journaled for it — cannot verify the pointer's disappearance is this \
                     operation's own doing"
                )));
            }
            (None, _) => {
                reset_pointer(state_dir, new_voyage, None)?;
            }
        },
        PointerState::Valid(_) => {
            return Err(err_state(format!(
                "reset {op_id}: the pointer names a THIRD identity — an operator must investigate; \
                 minting yet another would be exactly the double-mint at-most-once forbids"
            )));
        }
        PointerState::Corrupt | PointerState::OtherIo(_) => {
            return Err(err_state(format!("reset {op_id}: the pointer is unreadable during recovery")));
        }
    }
    journal::finish(
        state_dir,
        op_id,
        &journal::TerminalRecord::ResetDone { new_voyage: new_voyage.to_string() },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_pointer_renames_the_old_one_aside_and_mints_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        reset_pointer(dir.path(), &new_voyage, None).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == new_voyage));
        assert!(voyage_root_path(dir.path(), &new_voyage).exists());
        // The old voyage's own store is untouched -- only the POINTER
        // moved, never the data.
        assert!(voyage_root_path(dir.path(), &old).exists());
    }

    #[test]
    fn reset_pointer_uses_the_exact_journaled_aside_name() {
        let dir = tempfile::tempdir().unwrap();
        discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let aside_name = "drawer.voyage.reset-deadbeefdeadbeef";
        reset_pointer(dir.path(), &new_voyage, Some(aside_name)).unwrap();
        assert!(dir.path().join(aside_name).exists(), "the pre-chosen aside name must be exactly what's used");
    }

    #[test]
    fn reconcile_reset_recovers_all_four_states() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let aside = "drawer.voyage.reset-cafefacecafeface".to_string();

        // Row 1: pointer still names the OLD voyage -- resume from the
        // beginning.
        reconcile_reset(dir.path(), "op-1", &new_voyage, Some(&old), Some(&aside)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == new_voyage));
        assert_eq!(
            journal_state(dir.path(), "op-1"),
            Some(journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        );
        assert!(dir.path().join(&aside).exists(), "the journaled aside name must be exactly what got used");

        // Row 3: pointer already names the INTENDED NEW voyage -- just
        // reconstruct the terminal fact.
        reconcile_reset(dir.path(), "op-2", &new_voyage, Some(&old), Some(&aside)).unwrap();
        assert_eq!(
            journal_state(dir.path(), "op-2"),
            Some(journal::TerminalRecord::ResetDone { new_voyage: new_voyage.clone() })
        );

        // Row 4: pointer names something else entirely -- loud stop.
        let rogue = uuid::Uuid::now_v7().to_string();
        assert!(reconcile_reset(dir.path(), "op-3", &rogue, Some(&old), Some(&aside)).is_err());

        // Row 2: pointer ABSENT with the evidence rename PRESENT --
        // resume from publication. Codex review round 2: the row's own
        // setup must actually MATERIALIZE the file it claims exists.
        std::fs::remove_file(pointer::pointer_path(dir.path())).unwrap();
        let third = uuid::Uuid::now_v7().to_string();
        let aside2 = "drawer.voyage.reset-0000000000000001".to_string();
        std::fs::write(dir.path().join(&aside2), b"drawer.voyage").unwrap();
        reconcile_reset(dir.path(), "op-4", &third, Some(&new_voyage), Some(&aside2)).unwrap();
        assert!(matches!(pointer::validate(dir.path()), PointerState::Valid(v) if v == third));
    }

    #[test]
    fn reconcile_reset_refuses_when_the_pointer_is_absent_but_no_evidence_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let old = discover_or_mint_voyage(dir.path(), StartMode::Start).unwrap();
        std::fs::remove_file(pointer::pointer_path(dir.path())).unwrap();
        let new_voyage = uuid::Uuid::now_v7().to_string();
        let never_written = "drawer.voyage.reset-ffffffffffffffff";
        let err = reconcile_reset(dir.path(), "op-1", &new_voyage, Some(&old), Some(never_written)).unwrap_err();
        assert!(format!("{err}").contains("investigate"));
    }

    fn journal_state(state_dir: &Path, op_id: &str) -> Option<journal::TerminalRecord> {
        journal::read_terminal(state_dir, op_id).unwrap()
    }
}
