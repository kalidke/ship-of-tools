//! Declaration parsing, preservation, argument and exclusive-creation controls.
use super::*;

#[test]
fn typed_reader_accepts_toml_strings_and_ignores_other_schemas() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("settings.toml");
    for text in [
        "[trust] # scope\nroot_prefix = '/scope/#literal' # comment\n",
        "[ trust ]\nroot_prefix = \"/scope/\\u0023literal\"\n",
        "trust = { root_prefix = '/scope/#literal' }\n",
        "trust.root_prefix = '/scope/#literal'\n[other]\nvalue = [1, 2]\n",
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(
            read_trust_declaration(&file).unwrap(),
            Some(PathBuf::from("/scope/#literal"))
        );
    }
    for text in [
        "",
        "[other]\nroot_prefix = 7\n",
        "[trust]\n",
        "[trust]\nroot_prefix = ''\n",
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(read_trust_declaration(&file).unwrap(), None);
    }
    for text in ["[trust\n", "trust = false\n", "[trust]\nroot_prefix = 7\n"] {
        std::fs::write(&file, text).unwrap();
        assert!(read_trust_declaration(&file)
            .unwrap_err()
            .contains("settings.toml"));
    }
}

#[test]
fn every_existing_trust_answer_is_kept_byte_identical() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("settings.toml");
    let prefix = std::fs::canonicalize(temp.path()).unwrap();
    for text in [
        "[trust]\n",
        "[trust] # kept\nroot_prefix = ''\n",
        "trust = { root_prefix = '/kept' }\n",
        "trust.root_prefix = '/kept'\n",
        "trust = false\n",
        "[ trust ]\nroot_prefix = '/kept'\n",
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(
            declare_trust(&file, &prefix).unwrap(),
            DeclarationOutcome::Kept
        );
        assert_eq!(std::fs::read(&file).unwrap(), text.as_bytes());
    }
    for bytes in [&b"[trust"[..], &b"\xff\xfe"[..]] {
        std::fs::write(&file, bytes).unwrap();
        assert!(declare_trust(&file, &prefix).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
    }
}

#[test]
fn appearing_destination_wins_exclusive_creation() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("settings.toml");
    let prefix = std::fs::canonicalize(temp.path()).unwrap();
    let before = b"# race winner\n[layout]\npreset = 'wide'\n";
    let outcome = declare_with(&file, &prefix, || {
        let staged: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(staged.len(), 1);
        // The owner closed its staged file before this publication barrier.
        let staged_path = staged[0].path();
        let renamed = staged_path.with_extension("closed");
        std::fs::rename(&staged_path, &renamed).unwrap();
        std::fs::rename(&renamed, &staged_path).unwrap();
        std::fs::write(&file, before).unwrap();
    })
    .unwrap();
    assert_eq!(outcome, DeclarationOutcome::Kept);
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
    println!("W1 P6 owner PASS: race winner preserved; Kept; owned temporary file cleaned");
}

#[test]
fn failed_publication_cleans_only_the_owned_temp() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("settings.toml");
    let prefix = std::fs::canonicalize(temp.path()).unwrap();
    let result = declare_with(&file, &prefix, || {
        std::fs::create_dir(&file).unwrap();
        std::fs::write(file.join("winner"), b"kept").unwrap();
    });
    assert!(result.is_err() || result == Ok(DeclarationOutcome::Kept));
    assert_eq!(std::fs::read(file.join("winner")).unwrap(), b"kept");
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1);
}

#[test]
fn a_prefix_with_no_name_is_refused() {
    assert!(validate_prefix(Path::new("/")).is_err());
    #[cfg(windows)]
    for root in [r"C:\", r"\\?\C:\", r"\\?\UNC\host\share\", r"\\host\share\"] {
        assert!(validate_prefix(Path::new(root)).is_err(), "{root}");
    }
}
