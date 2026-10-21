use super::*;
use crate::spawn::tests::{output, RecordingSpawner};

fn archive_spawner(build: std::path::PathBuf) -> impl Spawner {
    RecordingSpawner(move |command: &tokio::process::Command| {
        let cmd = command.as_std();
        assert_eq!(cmd.get_program(), "tar", "unexpected archive command");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if args[0] == "-tzf" {
            return Ok(output(
                0,
                format!("{TOP}/\n{TOP}/sot\n{TOP}/sotd\n{TOP}/sot-capsule\n"),
            ));
        }
        assert_eq!(args[0], "-xzf");
        assert_eq!(args[2], "-C");
        let dest = Path::new(&args[3]).join(TOP);
        std::fs::create_dir_all(&dest)?;
        for entry in std::fs::read_dir(build.join(TOP))? {
            let entry = entry?;
            #[allow(clippy::disallowed_methods, reason = "a data copy of the fixture build, no program is run from it")]
            std::fs::copy(entry.path(), dest.join(entry.file_name()))?;
        }
        Ok(output(0, Vec::new()))
    })
}

const TOP: &str = "sot-0.6.0-linux-x86_64";

#[test]
fn entry_validation() {
    validate_entry_name("sot-0.6.0-linux-x86_64/", TOP).unwrap();
    validate_entry_name("sot-0.6.0-linux-x86_64", TOP).unwrap();
    validate_entry_name("sot-0.6.0-linux-x86_64/sot", TOP).unwrap();
    validate_entry_name("sot-0.6.0-linux-x86_64/sotd", TOP).unwrap();
    validate_entry_name("sot-0.6.0-linux-x86_64/sotd.service", TOP).unwrap();
    // Unknown-but-sane files are tolerated: future releases add files,
    // and deployed updaters must not refuse to stage them.
    validate_entry_name("sot-0.6.0-linux-x86_64/sot-apply", TOP).unwrap();
    validate_entry_name("sot-0.6.0-linux-x86_64/NEW_FILE.txt", TOP).unwrap();

    for bad in [
        "/etc/passwd",
        "../../evil",
        "sot-0.6.0-linux-x86_64/../evil",
        "sot-0.6.0-linux-x86_64/nested/sot",
        "other-dir/sot",
        "sot-0.6.0-linux-x86_64\\sot",
        "./sot-0.6.0-linux-x86_64/sot",
        "sot-0.6.0-linux-x86_64/.hidden",
        "sot-0.6.0-linux-x86_64/sp ace",
    ] {
        assert!(validate_entry_name(bad, TOP).is_err(), "accepted {bad:?}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn tar_roundtrip_extracts_and_validates() {
    let base = std::env::temp_dir().join(format!("sot-updater-arch-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&base).await;
    let stage = base.join("build").join(TOP);
    tokio::fs::create_dir_all(&stage).await.unwrap();
    tokio::fs::write(stage.join("sot"), b"fe").await.unwrap();
    tokio::fs::write(stage.join("sotd"), b"be").await.unwrap();
    // ADR 0042 slice L1a: `sot-capsule` is required now too (see
    // `validate_tree`'s own comment) -- omitting it here would make
    // this "complete archive" fixture fail its own success assertion
    // below.
    tokio::fs::write(stage.join("sot-capsule"), b"cap")
        .await
        .unwrap();
    tokio::fs::write(stage.join("sotd.service"), b"unit")
        .await
        .unwrap();
    let archive = base.join(format!("{TOP}.tar.gz"));
    tokio::fs::write(&archive, b"synthetic archive")
        .await
        .unwrap();
    let spawner = archive_spawner(base.join("build"));

    let entries = list_entries(&spawner, &archive).await.unwrap();
    assert!(entries.iter().any(|e| e.trim_end_matches('/') == TOP));

    let dest = base.join("out");
    tokio::fs::create_dir_all(&dest).await.unwrap();
    extract_validated(&spawner, &archive, &dest, TOP)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dest.join(TOP).join("sot")).await.unwrap(),
        b"fe"
    );

    // A missing required binary fails the post-extraction sweep.
    tokio::fs::remove_file(base.join("build").join(TOP).join("sotd"))
        .await
        .unwrap();
    let bad = base.join("bad.tar.gz");
    tokio::fs::write(&bad, b"synthetic incomplete archive")
        .await
        .unwrap();
    let dest2 = base.join("out2");
    tokio::fs::create_dir_all(&dest2).await.unwrap();
    let err = extract_validated(&spawner, &bad, &dest2, TOP)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("missing required file"), "{err}");

    tokio::fs::remove_dir_all(&base).await.unwrap();
}
