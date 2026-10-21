use super::*;

const T: &str = "linux-x86_64";

fn identity(ver: &str) -> ReleaseIdentity {
    ReleaseIdentity {
        repo: "kalidke/ship-of-tools".into(),
        tag: format!("v{ver}"),
        version: ver.into(),
        target: T.into(),
        asset: format!("sot-{ver}-{T}.tar.gz"),
        asset_sha256: "cd".repeat(32),
    }
}

/// A fresh `<prefix>/updates` (returned) beside a `<prefix>/install.json` naming `installed`, or none.
async fn prefix(name: &str, installed: Option<(&str, &str)>) -> PathBuf {
    let prefix = std::env::temp_dir().join(format!("sot-updater-{name}-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&prefix).await;
    tokio::fs::create_dir_all(prefix.join("updates")).await.unwrap();
    if let Some((version, commit)) = installed {
        install(&prefix, version, commit, None).await;
    }
    prefix.join("updates")
}

/// Write `<prefix>/install.json` naming `version`, with `pinned` only when given. serde writes it, so a prefix holding
/// a `\` (every Windows path) or a `"` is escaped as an installer escapes it, and the install must read back: a
/// manifest that does not parse reads as no install, and every arm over it would pass for the wrong reason.
async fn install(prefix: &Path, version: &str, commit: &str, pinned: Option<bool>) {
    let mut json = serde_json::json!({
        "schema": 1, "prefix": prefix, "version": version, "tag": format!("v{version}"), "commit": commit,
    });
    if let Some(pinned) = pinned {
        json["pinned"] = pinned.into();
    }
    tokio::fs::write(prefix.join(INSTALL_MANIFEST), json.to_string()).await.unwrap();
    let read = InstalledBuild::try_read(&prefix.join("updates")).unwrap().expect("the fixture's install.json reads back");
    assert_eq!((read.version.as_str(), read.pinned), (version, pinned == Some(true)));
}

async fn cleanup(root: &Path) {
    tokio::fs::remove_dir_all(root.parent().unwrap()).await.unwrap();
}

#[tokio::test]
async fn arm_read_clear_newer_wins_and_bad_marker() {
    let root = prefix("pending", None).await;

    assert!(read(&root, T).await.unwrap().is_none());

    let co = PathBuf::from("/x/repo/versions/v0.6.0");
    let offer = |v: &'static str, c: &'static str| {
        let (root, co) = (root.clone(), co.clone());
        async move { arm(&root, &identity(v), &co, c, Writer::Automatic).await.unwrap() }
    };
    assert_eq!(offer("0.6.0", "aaa").await, ArmOutcome::Armed);
    let got = read(&root, T).await.unwrap().unwrap();
    assert_eq!(got.identity.tag, "v0.6.0");
    // Another target sees nothing.
    assert!(read(&root, "macos-aarch64").await.unwrap().is_none());

    // Newer replaces older…
    assert_eq!(offer("0.7.0", "bbb").await, ArmOutcome::Armed);
    // …but an older late-finisher cannot clobber a newer arm.
    assert_eq!(offer("0.6.1", "ccc").await, ArmOutcome::KeptNewer);
    assert_eq!(read(&root, T).await.unwrap().unwrap().identity.tag, "v0.7.0");

    // A bad-marked version refuses to arm.
    std::fs::write(bad_marker(&root, "v0.8.0", T), b"").unwrap();
    assert_eq!(offer("0.8.0", "ddd").await, ArmOutcome::MarkedBad);

    cleanup(&root).await;
}

#[tokio::test]
async fn an_automatic_arm_goes_only_above_the_install_and_records_it() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.13");
    // The red this pins: a stale process offers rc9.13 into an install that already moved past it.
    for installed in ["0.6.6-rc9.13", "0.6.6-rc9.14"] {
        let root = prefix("below", Some((installed, "c0"))).await;
        let out = arm(&root, &identity("0.6.6-rc9.13"), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::NotAboveInstall, "installed {installed}");
        assert!(read(&root, T).await.unwrap().is_none(), "installed {installed}: nothing armed");
        cleanup(&root).await;
    }
    // A dev build takes no release from an automatic writer, whether below or above it.
    for offered in ["0.6.6-rc9.13", "0.6.6-rc9.14", "0.6.7"] {
        let root = prefix("dev", Some(("0.6.6-rc9.14-dev+d94ed0fac", "c0"))).await;
        let out = arm(&root, &identity(offered), &co, "cc", Writer::Automatic).await.unwrap();
        assert_eq!(out, ArmOutcome::DevInstall, "offered {offered}");
        assert!(read(&root, T).await.unwrap().is_none(), "offered {offered}: nothing armed");
        cleanup(&root).await;
    }

    let root = prefix("above", Some(("0.6.6-rc9.13", "c0"))).await;
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    let ptr = read(&root, T).await.unwrap().unwrap();
    assert_eq!(ptr.armed_over_version.as_deref(), Some("0.6.6-rc9.13"));
    assert_eq!(ptr.armed_over_commit.as_deref(), Some("c0"));
    let text = tokio::fs::read_to_string(pointer_path(&root, T)).await.unwrap();
    assert!(text.contains("\"armed_over_version\": \"0.6.6-rc9.13\""), "{text}");
    cleanup(&root).await;
}

// The run that named a version pinned the install: no release is armed over it automatically, the operator's still is.
#[tokio::test]
async fn a_pinned_install_takes_no_automatic_arm() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
    let root = prefix("pinned", Some(("0.6.6-rc9.13", "c0"))).await;
    install(root.parent().unwrap(), "0.6.6-rc9.13", "c0", Some(true)).await;
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
    assert_eq!(out, ArmOutcome::Pinned);
    assert!(read(&root, T).await.unwrap().is_none(), "nothing armed over a pinned install");
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Operator).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed, "the operator's arm still moves it");
    cleanup(&root).await;
}

// An install.json that cannot be read may say `"pinned": true`: an automatic arm refuses over it, the operator's does not.
#[tokio::test]
async fn an_unreadable_install_takes_no_automatic_arm() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
    let root = prefix("unreadable", None).await;
    let file = root.parent().unwrap().join(INSTALL_MANIFEST);
    tokio::fs::write(&file, r#"{"schema":1,"version":"0.6.6-rc9.13","pinned":tr"#).await.unwrap();
    assert!(InstalledBuild::try_read(&root).is_err());
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
    assert!(matches!(out, ArmOutcome::UnreadableInstall(_)), "{out:?}");
    assert!(read(&root, T).await.unwrap().is_none(), "nothing armed over an install that cannot be read");
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Operator).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed, "the operator's arm proceeds");
    cleanup(&root).await;
}

// A BOM (a hand edit saved by Notepad) is not unreadable, and an absent install.json is still no install.
#[tokio::test]
async fn a_bom_led_install_reads_and_an_absent_one_is_no_install() {
    let root = prefix("bom", None).await;
    assert_eq!(InstalledBuild::try_read(&root), Ok(None), "absent: no install");
    let text = r#"{"schema":1,"prefix":"/p","version":"0.6.6-rc9.13","commit":"c0","pinned":true}"#;
    tokio::fs::write(root.parent().unwrap().join(INSTALL_MANIFEST), format!("\u{feff}{text}")).await.unwrap();
    let read = InstalledBuild::try_read(&root).unwrap().unwrap();
    assert_eq!((read.version.as_str(), read.pinned), ("0.6.6-rc9.13", true));
    cleanup(&root).await;
}

// `"pinned": true` with no version to pin cannot be told from a hand edit gone wrong: an automatic writer treats it
// as unreadable, the operator's arm proceeds, and an unpinned record with no version is still no install.
#[tokio::test]
async fn a_pin_with_no_version_is_unreadable_to_an_automatic_writer() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
    let root = prefix("pinnoversion", None).await;
    let file = root.parent().unwrap().join(INSTALL_MANIFEST);
    tokio::fs::write(&file, r#"{"schema":1,"prefix":"/p","pinned":true}"#).await.unwrap();
    assert!(InstalledBuild::try_read(&root).is_err());
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
    assert!(matches!(out, ArmOutcome::UnreadableInstall(_)), "{out:?}");
    assert_eq!(arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Operator).await.unwrap(), ArmOutcome::Armed);
    tokio::fs::write(&file, r#"{"schema":1,"prefix":"/p"}"#).await.unwrap();
    assert_eq!(InstalledBuild::try_read(&root), Ok(None), "no version and no pin: no install");
    cleanup(&root).await;
}

// The pointer's own judgment over an install that cannot be read: a pointer armed over a record cannot be checked.
#[test]
fn a_pointer_armed_over_a_record_is_refused_when_the_install_cannot_be_read() {
    let ptr = |over: Option<&str>| PendingPointer {
        schema: 1,
        identity: identity("0.6.6-rc9.14"),
        checkout: PathBuf::from("/x"),
        commit: "cc".into(),
        armed_at: 0,
        armed_over_version: over.map(Into::into),
        armed_over_commit: None,
    };
    let cut = UnreadableInstall { path: PathBuf::from("/p/install.json"), cause: "EOF while parsing".into(), missing: false };
    let why = pointer_refusal(&ptr(Some("0.6.6-rc9.13")), Err(cut.clone())).expect("refused");
    assert!(why.contains("0.6.6-rc9.13") && why.contains(&cut.sentence()), "{why}");
    assert!(pointer_refusal(&ptr(None), Err(cut)).is_none(), "no record: the script's exact-tag check judges it");
    assert!(pointer_refusal(&ptr(Some("0.6.6-rc9.13")), Ok(None)).is_none(), "no install: nothing to compare");
}

// Item 9: on Windows a missing record beside bin\sotd.exe is refused like a cut one (a writer's replace can leave it
// briefly missing); elsewhere, and with no install beside it, absent is no install.
#[tokio::test]
async fn a_missing_install_json_beside_a_windows_install_takes_no_automatic_arm() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
    let root = prefix("missing", None).await;
    let bin = root.parent().unwrap().join("bin");
    assert_eq!(InstalledBuild::try_read(&root), Ok(None), "absent with no install beside it: no install");
    tokio::fs::create_dir_all(&bin).await.unwrap();
    tokio::fs::write(bin.join("sotd.exe"), b"").await.unwrap();
    let auto = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
    if cfg!(windows) {
        let missing = InstalledBuild::try_read(&root).unwrap_err();
        let said = missing.sentence();
        assert!(missing.missing && said.starts_with(crate::manifest::MISSING_LEAD), "{said}");
        assert!(said.contains("install.ps1 -Version") && !said.contains("repair"), "{said}");
        assert!(matches!(auto, ArmOutcome::UnreadableInstall(_)), "{auto:?}");
        assert!(read(&root, T).await.unwrap().is_none(), "nothing armed");
        let op = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Operator).await.unwrap();
        assert_eq!(op, ArmOutcome::Armed, "the operator's arm proceeds");
    } else {
        assert_eq!(InstalledBuild::try_read(&root), Ok(None), "Unix: absent stays no install");
        assert_eq!(auto, ArmOutcome::Armed);
    }
    cleanup(&root).await;
}

// Item 9's measurement (review N1): while a writer replaces a pinned record 3000 times with the shipped helper, no
// automatic arm passes the pin. Without the rule an arm that meets the record missing takes it for no install.
#[cfg(windows)]
#[tokio::test]
async fn no_automatic_arm_passes_a_pin_while_the_record_is_replaced() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
    let root = prefix("race", None).await;
    let dir = root.parent().unwrap().to_path_buf();
    tokio::fs::create_dir_all(dir.join("bin")).await.unwrap();
    tokio::fs::write(dir.join("bin").join("sotd.exe"), b"").await.unwrap();
    install(&dir, "0.6.6-rc9.13", "c0", Some(true)).await;
    let layout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/sot-install-layout.ps1");
    let record = dir.join(INSTALL_MANIFEST);
    let script = format!(
        "$ErrorActionPreference = 'Stop'; . '{}'; $t = [IO.File]::ReadAllText('{}'); for ($i = 0; $i -lt 3000; $i++) {{ Write-Utf8NoBomViaRename '{}' $t }}",
        layout.display(),
        record.display(),
        record.display()
    );
    let writer = std::thread::spawn(move || {
        sot_log::test_wait::run_within(
            std::process::Command::new("powershell.exe").args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script]),
            Duration::from_secs(600),
        )
        .ok();
    });
    let (mut polls, mut refused) = (0u32, 0u32);
    while !writer.is_finished() {
        match arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap() {
            ArmOutcome::Pinned => {}
            ArmOutcome::UnreadableInstall(_) => refused += 1,
            other => panic!("an automatic arm passed the pin on poll {polls}: {other:?}"),
        }
        polls += 1;
    }
    writer.join().unwrap();
    println!("{polls} automatic arms while the record was replaced 3000 times; {refused} found it missing and refused");
    assert!(polls > 0 && read(&root, T).await.unwrap().is_none(), "nothing armed over the pin");
    cleanup(&root).await;
}

// An install.json with no pin field (every install made before the field existed) is unpinned and follows its channel.
#[tokio::test]
async fn an_install_with_no_pin_field_follows_its_channel() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.14");
    let root = prefix("nopin", Some(("0.6.6-rc9.13", "c0"))).await;
    assert!(!InstalledBuild::try_read(&root).unwrap().unwrap().pinned, "no field reads as unpinned");
    assert!(InstalledBuild::try_read(&root).unwrap().unwrap().admits("0.6.6-rc9.14"));
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cc", Writer::Automatic).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    cleanup(&root).await;
    // An explicit false is unpinned too.
    let root = prefix("pinfalse", Some(("0.6.6-rc9.13", "c0"))).await;
    install(root.parent().unwrap(), "0.6.6-rc9.13", "c0", Some(false)).await;
    assert!(!InstalledBuild::try_read(&root).unwrap().unwrap().pinned);
    cleanup(&root).await;
}

#[tokio::test]
async fn the_operator_may_arm_the_same_or_an_older_build() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.13");
    let root = prefix("operator", Some(("0.6.6-rc9.14-dev+abc", "c0"))).await;
    let out = arm(&root, &identity("0.6.6-rc9.13"), &co, "cc", Writer::Operator).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    let ptr = read(&root, T).await.unwrap().unwrap();
    assert_eq!(ptr.armed_over_version.as_deref(), Some("0.6.6-rc9.14-dev+abc"));
    // The same stamp, another build of it.
    let out = arm(&root, &identity("0.6.6-rc9.14-dev+abd"), &co, "cd", Writer::Operator).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    cleanup(&root).await;
}

#[tokio::test]
async fn the_operators_arm_replaces_a_newer_pointer() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.16");
    let root = prefix("operator-newer", Some(("0.6.6-rc9.13", "c0"))).await;
    let out = arm(&root, &identity("0.6.6-rc9.16"), &co, "cc", Writer::Automatic).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    // An automatic late finisher loses to it; the operator does not.
    let late = arm(&root, &identity("0.6.6-rc9.14"), &co, "cd", Writer::Automatic).await.unwrap();
    assert_eq!(late, ArmOutcome::KeptNewer);
    let out = arm(&root, &identity("0.6.6-rc9.14"), &co, "cd", Writer::Operator).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    assert_eq!(read(&root, T).await.unwrap().unwrap().identity.tag, "v0.6.6-rc9.14");
    cleanup(&root).await;
}

#[tokio::test]
async fn a_pointer_that_no_longer_fits_the_install_does_not_hold_the_slot() {
    let co = PathBuf::from("/x/repo/versions/v0.6.6-rc9.15");
    let root = prefix("refit", Some(("0.6.6-rc9.13", "c0"))).await;
    let out = arm(&root, &identity("0.6.6-rc9.16"), &co, "cc", Writer::Automatic).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    // The install moves to another commit of rc9.14; the pointer was armed over rc9.13.
    let json = r#"{"schema":1,"prefix":"/p","version":"0.6.6-rc9.14","commit":"c1"}"#;
    tokio::fs::write(root.parent().unwrap().join(INSTALL_MANIFEST), json).await.unwrap();
    // A lower release, still above the install, replaces the stale pointer instead of losing to it.
    let out = arm(&root, &identity("0.6.6-rc9.15"), &co, "cd", Writer::Automatic).await.unwrap();
    assert_eq!(out, ArmOutcome::Armed);
    let ptr = read(&root, T).await.unwrap().unwrap();
    assert_eq!(ptr.identity.tag, "v0.6.6-rc9.15");
    assert_eq!(ptr.armed_over_commit.as_deref(), Some("c1"));
    cleanup(&root).await;
}

#[test]
fn stale_reason_judges_a_pointer_by_its_record_or_by_the_version() {
    let ptr = |version: &str, over: Option<(&str, &str)>| PendingPointer {
        schema: 1,
        identity: identity(version),
        checkout: PathBuf::from("/x"),
        commit: "cc".into(),
        armed_at: 0,
        armed_over_version: over.map(|o| o.0.into()),
        armed_over_commit: over.map(|o| o.1.into()),
    };
    let at = |version: &str, commit: &str| InstalledBuild { version: version.into(), commit: Some(commit.into()), pinned: false };
    // A record: the install is still the one it was armed over.
    assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0"))), &at("0.6.6-rc9.13", "c0")).is_none());
    // A same-stamp operator pointer is fine while the install is unchanged.
    assert!(stale_reason(&ptr("0.6.6-rc9.14-dev+a", Some(("0.6.6-rc9.14-dev+b", "c0"))), &at("0.6.6-rc9.14-dev+b", "c0")).is_none());
    // The install moved: another version, or another commit of the same version.
    assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0"))), &at("0.6.6-rc9.14", "c0")).is_some());
    assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.13", "c0"))), &at("0.6.6-rc9.13", "c1")).is_some());
    // No record: only strictly above the install applies.
    assert!(stale_reason(&ptr("0.6.6-rc9.14", None), &at("0.6.6-rc9.13", "c0")).is_none());
    assert!(stale_reason(&ptr("0.6.6-rc9.13", None), &at("0.6.6-rc9.14-dev+d94", "c0")).is_some());
    assert!(stale_reason(&ptr("0.6.6-rc9.13", None), &at("0.6.6-rc9.13", "c0")).is_some());
    // Never onto a dev build, even a release above its stamp.
    let dev = at("0.6.6-rc9.14-dev+d94ed0fac", "c1");
    assert!(stale_reason(&ptr("0.6.6-rc9.14", None), &dev).unwrap().contains("dev build"));
    assert!(stale_reason(&ptr("0.6.7", None), &dev).is_some());
    // An operator's pointer carries a record, and that is what it is judged by.
    assert!(stale_reason(&ptr("0.6.6-rc9.14", Some(("0.6.6-rc9.14-dev+d94ed0fac", "c1"))), &dev).is_none());
}

#[tokio::test]
async fn corrupt_pointer_is_an_error_not_none() {
    let root =
        std::env::temp_dir().join(format!("sot-updater-pending-bad-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&root).await;
    tokio::fs::create_dir_all(&root).await.unwrap();
    tokio::fs::write(pointer_path(&root, T), b"{not json")
        .await
        .unwrap();
    assert!(read(&root, T).await.is_err());
    tokio::fs::remove_dir_all(&root).await.unwrap();
}
