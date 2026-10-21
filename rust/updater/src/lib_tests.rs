use super::*;
use crate::spawn::tests::{output, RecordingSpawner};

fn archive_spawner(fixture_build: PathBuf, fixture_top: String) -> impl Spawner {
    RecordingSpawner(move |command: &tokio::process::Command| {
        let cmd = command.as_std();
        assert_eq!(cmd.get_program(), "tar", "unexpected stage command");
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        if args[0] == "-tzf" {
            return Ok(output(0, format!("{fixture_top}/\n{fixture_top}/sot\n{fixture_top}/sotd\n{fixture_top}/sot-capsule\n")));
        }
        assert_eq!(args[0], "-xzf");
        assert_eq!(args[2], "-C");
        let dest = Path::new(&args[3]).join(&fixture_top);
        std::fs::create_dir_all(&dest)?;
        for entry in std::fs::read_dir(&fixture_build)? {
            let entry = entry?;
            #[allow(clippy::disallowed_methods, reason = "a data copy of the fixture build, no program is run from it")]
            std::fs::copy(entry.path(), dest.join(entry.file_name()))?;
        }
        Ok(output(0, Vec::new()))
    })
}

/// Build a fake release dir (SHA256SUMS + one platform archive) and run
/// the full check → stage flow against it via the Dir fetcher.
#[tokio::test]
async fn check_and_stage_end_to_end() {
    let base = std::env::temp_dir().join(format!("sot-updater-e2e-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&base).await;
    let release = base.join("release");
    let updates = base.join("updates");
    tokio::fs::create_dir_all(&release).await.unwrap();

    let target = platform::this_platform().expect("test host must be in the release matrix");
    let version = "9.9.9";
    let asset = platform::platform_asset(version).unwrap();
    let top = format!("sot-{version}-{target}");

    // Assemble the archive exactly like release CI does.
    let build = base.join("build").join(&top);
    tokio::fs::create_dir_all(&build).await.unwrap();
    tokio::fs::write(build.join("sot"), b"fe-binary")
        .await
        .unwrap();
    tokio::fs::write(build.join("sotd"), b"be-binary")
        .await
        .unwrap();
    // ADR 0042 slice L1a: `sot-capsule` is a required file in every
    // release archive now (see `archive.rs`'s own `validate_tree`).
    tokio::fs::write(build.join("sot-capsule"), b"capsule-binary")
        .await
        .unwrap();
    tokio::fs::write(build.join("sotd.service"), b"unit")
        .await
        .unwrap();
    let archive = release.join(&asset);
    tokio::fs::write(&archive, b"synthetic archive")
        .await
        .unwrap();
    let spawner = archive_spawner(build.clone(), top.clone());
    let digest = fetch::hash::sha256_file(&archive).await.unwrap();
    tokio::fs::write(release.join("SHA256SUMS"), format!("{digest}  {asset}\n"))
        .await
        .unwrap();

    let cfg = UpdaterConfig {
        repo: "example/project".into(),
        current_version: "0.1.0".into(),
        fetcher: Fetcher::Dir(release.clone()),
        updates_root: updates.clone(),
    };

    let out = check_release(&spawner, &cfg.repo, &cfg.current_version, &cfg.fetcher).await;
    assert_eq!(out.status, "ok");
    assert!(out.update_available);
    let id = out.identity.unwrap();
    assert_eq!(id.tag, format!("v{version}"));
    assert_eq!(id.asset, asset);

    // A wrong pinned digest refuses to stage and leaves nothing ready.
    let mut bad = id.clone();
    bad.asset_sha256 = "0".repeat(64);
    assert!(stage(&spawner, &cfg, &bad).await.is_err());
    assert!(!is_staged(&updates, &bad).await);

    assert!(!is_staged(&updates, &id).await);
    assert!(stage(&spawner, &cfg, &id).await.unwrap());
    assert!(is_staged(&updates, &id).await);
    // Idempotent.
    assert!(stage(&spawner, &cfg, &id).await.unwrap());

    let staged_bin = stage_dir(&updates, &id).join(&top).join("sot");
    assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");
    let manifest = ReadyManifest::read(&stage_dir(&updates, &id))
        .await
        .unwrap();
    assert_eq!(manifest.identity, id);

    // A running check against the staged version reports no update.
    let cfg_current = UpdaterConfig {
        current_version: version.into(),
        ..cfg.clone()
    };
    let out2 = check_release(
        &spawner,
        &cfg_current.repo,
        &cfg_current.current_version,
        &cfg_current.fetcher,
    )
    .await;
    assert_eq!(out2.status, "ok");
    assert!(!out2.update_available);

    // A converge restart kills the frontend's staging task mid-download.
    // What the next attempt may pick up is bounded by one rule: only the
    // asset, and only on a full-content digest match.
    let partial = updates.join(format!("tmp-{}-{}", id.tag, id.target));
    let asset_len = tokio::fs::metadata(&archive).await.unwrap().len();

    // Bytes that are NOT this release's are discarded, not trusted — the
    // truncated-download case, which a size or existence test would pass.
    tokio::fs::remove_dir_all(stage_dir(&updates, &id))
        .await
        .unwrap();
    tokio::fs::create_dir_all(&partial).await.unwrap();
    tokio::fs::write(partial.join(&asset), b"half a download")
        .await
        .unwrap();
    assert!(stage(&spawner, &cfg, &id).await.unwrap());
    assert!(is_staged(&updates, &id).await);
    assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");

    // The verified asset IS reused. Proven by making a download
    // impossible: the asset is removed from the source the fetcher pulls
    // from, so an attempt that started again instead of continuing fails.
    tokio::fs::remove_dir_all(stage_dir(&updates, &id))
        .await
        .unwrap();
    tokio::fs::create_dir_all(&partial).await.unwrap();
    tokio::fs::copy(&archive, partial.join(&asset))
        .await
        .unwrap();
    tokio::fs::remove_file(&archive).await.unwrap();
    assert_eq!(partial_asset_bytes(&updates, &id).await, Some(asset_len));
    assert!(stage(&spawner, &cfg, &id).await.unwrap());
    assert!(is_staged(&updates, &id).await);
    assert_eq!(tokio::fs::read(&staged_bin).await.unwrap(), b"fe-binary");
    // Committing consumes the partial dir: nothing is left claiming to be
    // resumable once the stage it fed is complete.
    assert_eq!(partial_asset_bytes(&updates, &id).await, None);

    tokio::fs::remove_dir_all(&base).await.unwrap();
}
