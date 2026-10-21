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
    // resume from publication. The row's own
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
