use super::*;

const DROPIN: &str = "# Written by the Ship of Tools installer: this home is on a network filesystem, so the daemon keeps its\n# session records on this computer's own disk (ADR 0043 decision 23).\n[Service]\nEnvironment=XDG_STATE_HOME=/scratch/account/sot-state\n";

#[test]
fn state_folder_decisions_preserve_explicit_choices_and_personal_dropins() {
    let root = Path::new("/home/account/.local/state/sot");
    let dropin = Path::new("/home/account/.config/systemd/user/sotd.service.d/state.conf");
    for xdg in [None, Some(std::ffi::OsStr::new("/chosen/state"))] {
        for qualifies in [false, true] {
            for (scratch, parent, qualified, reason) in [
                (true, Parent::Owned, true, ""),
                (true, Parent::Absent, true, ""),
                (false, Parent::Absent, true, "/scratch is not a directory"),
                (
                    true,
                    Parent::Foreign,
                    true,
                    "/scratch/account is owned by another uid",
                ),
                (true, Parent::Link, true, "/scratch/account is a symlink"),
                (true, Parent::Owned, false, "candidate is on tmpfs"),
            ] {
                let candidate = Candidate {
                    base: PathBuf::from("/scratch/account/sot-state"),
                    scratch_dir: scratch,
                    parent,
                    qualified: if qualified {
                        Ok(PathBuf::from("/scratch/account/sot-state/sot"))
                    } else {
                        Err(reason.into())
                    },
                };
                for existing in [
                    None,
                    Some(DROPIN.as_bytes()),
                    Some(b"[Service]\nEnvironment=PERSONAL=1\n".as_slice()),
                ] {
                    let verdict = if qualifies {
                        Ok(root.to_path_buf())
                    } else {
                        Err("default is on NFS".into())
                    };
                    let decision = decide(xdg, root, verdict, Some(&candidate), dropin, existing);
                    if qualifies {
                        assert_eq!(decision, Choice::Ok(root.to_path_buf()));
                    } else if xdg.is_some() {
                        assert_eq!(decision, Choice::Todo(format!("state folder {}: default is on NFS; point XDG_STATE_HOME at a local disk where it is exported, then restart sotd", root.display())));
                    } else if existing.is_some_and(|b| b != DROPIN.as_bytes()) {
                        assert_eq!(decision, Choice::Todo(format!("state folder {}: default is on NFS; {} is a person's file: leave it and point its XDG_STATE_HOME at a local disk, then run systemctl --user daemon-reload && systemctl --user restart sotd.service", root.display(), dropin.display())));
                    } else if !reason.is_empty() {
                        assert_eq!(decision, Choice::Todo(format!("state folder {}: default is on NFS; {reason}; make a folder on a local disk and write {} with it: mkdir -p /path/on/local/disk/state && chmod 700 /path/on/local/disk/state; mkdir -p {} && printf '[Service]\\nEnvironment=XDG_STATE_HOME=/path/on/local/disk/state\\n' > {}; systemctl --user daemon-reload && systemctl --user restart sotd.service", root.display(), dropin.display(), dropin.parent().unwrap().display(), dropin.display())));
                    } else {
                        assert_eq!(
                            decision,
                            Choice::Made {
                                root: PathBuf::from("/scratch/account/sot-state/sot"),
                                text: DROPIN.into()
                            }
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn without_a_managed_unit_an_unqualified_root_is_a_step() {
    let root = Path::new("/state/sot");
    let dropin = Path::new("/units/sotd.service.d/state.conf");
    let choice = decide(None, root, Err("not durable".into()), None, dropin, None);
    let mut found = super::super::verdict::Found::default();
    choice.record(&mut found);
    let (text, code) = super::super::verdict::line(&found, "");
    assert_eq!(code, 0);
    assert_eq!(text, "ONE STEP LEFT: state folder /state/sot: not durable; point XDG_STATE_HOME at a local disk where it is exported, then restart sotd");
}

fn bad_home(dir: &Path) -> Env {
    use std::os::unix::fs::PermissionsExt;
    let home = dir.join("home");
    let root = home.join(".local/state/sot");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    Env::empty().with("HOME", home)
}

#[test]
fn a_qualified_root_needs_no_unit_or_scratch_folder() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::empty().with("HOME", dir.path().join("home"));
    let scratch = dir.path().join("never-create");
    let units = dir.path().join("units");
    for unit in [None, Some(units.as_path())] {
        let (choice, changed) = setup(&env, unit, &scratch, "account").unwrap();
        assert_eq!(
            choice,
            Choice::Ok(
                dir.path()
                    .join("home/.local/state/sot")
                    .canonicalize()
                    .unwrap()
            )
        );
        assert!(!changed);
        assert!(!scratch.exists());
        assert!(!units.exists());
    }
}

#[test]
fn a_candidate_is_created_private_and_its_dropin_is_durable_and_repeatable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let env = bad_home(dir.path()).with("XDG_STATE_HOME", "");
    let scratch = dir.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let units = dir.path().join("units");
    let base = scratch.join("account/sot-state");
    let (choice, changed) = setup(&env, Some(&units), &scratch, "account").unwrap();
    assert_eq!(
        choice,
        Choice::Made {
            root: base.join("sot").canonicalize().unwrap(),
            text: dropin_text(&base)
        }
    );
    assert!(changed);
    assert_eq!(
        std::fs::metadata(base.join("sot"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(scratch.join("account"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read_to_string(units.join("sotd.service.d/state.conf")).unwrap(),
        dropin_text(&base)
    );
    let (repeated, changed) = setup(&env, Some(&units), &scratch, "account").unwrap();
    assert_eq!(repeated, choice);
    assert!(!changed);
}

#[test]
fn unsafe_scratch_parents_are_never_followed_or_used() {
    let dir = tempfile::tempdir().unwrap();
    let uid = sot_platform::host::state_dir::current_uid();
    let absent = candidate_at(&dir.path().join("absent"), "account", uid);
    assert!(absent.refusal().unwrap().ends_with("is not a directory"));
    assert!(!dir.path().join("absent").exists());
    std::fs::create_dir(dir.path().join("account")).unwrap();
    let foreign = candidate_at(dir.path(), "account", uid.wrapping_add(1));
    assert!(foreign
        .refusal()
        .unwrap()
        .ends_with("is owned by another uid"));
    assert!(!dir.path().join("account/sot-state").exists());
    std::fs::create_dir(dir.path().join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join("link")).unwrap();
    let link = candidate_at(dir.path(), "link", uid);
    assert!(link.refusal().unwrap().ends_with("is a symlink"));
    assert!(!dir.path().join("elsewhere/sot-state").exists());
}

#[test]
fn a_personal_dropin_and_an_explicit_xdg_value_stay_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let env = bad_home(dir.path());
    let scratch = dir.path().join("scratch");
    std::fs::create_dir(&scratch).unwrap();
    let units = dir.path().join("units");
    let dropin = units.join("sotd.service.d/state.conf");
    std::fs::create_dir_all(dropin.parent().unwrap()).unwrap();
    let own = b"[Service]\nEnvironment=XDG_STATE_HOME=/chosen/by/person\n";
    std::fs::write(&dropin, own).unwrap();
    let (choice, changed) = setup(&env, Some(&units), &scratch, "account").unwrap();
    assert!(matches!(choice, Choice::Todo(t) if t.contains("is a person's file")));
    assert!(!changed);
    assert!(!scratch.join("account").exists(), "nothing is made under the scratch folder for a person's drop-in");
    assert_eq!(std::fs::read(&dropin).unwrap(), own);
    assert_eq!(publish(&dropin, DROPIN).unwrap(), None);
    let link = dir.path().join("linked.conf");
    std::os::unix::fs::symlink(&dropin, &link).unwrap();
    assert_eq!(publish(&link, DROPIN).unwrap(), None);
    let explicit = env.clone().with(
        "XDG_STATE_HOME",
        PathBuf::from(env.get("HOME").unwrap()).join(".local/state"),
    );
    let untouched = dir.path().join("never-create");
    for units in [Some(units.as_path()), None] {
        let (choice, changed) = setup(&explicit, units, &untouched, "account").unwrap();
        assert!(
            matches!(choice, Choice::Todo(t) if t.contains("point XDG_STATE_HOME at a local disk where it is exported"))
        );
        assert!(!changed);
        assert!(!untouched.exists());
    }
    let (choice, changed) = setup(&env, None, &untouched, "account").unwrap();
    assert!(matches!(choice, Choice::Todo(_)));
    assert!(!changed);
    assert_eq!(std::fs::read(&dropin).unwrap(), own);
}
