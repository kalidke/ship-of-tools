//! What an automatic update works on, built from real programs: an upstream git repository with the release's tag, an
//! install-shaped prefix (a copy of the built daemon beside its capsule, an install manifest, a base clone of the
//! upstream) and a release folder (the platform archive, `SHA256SUMS` and `COMMIT`) for the updater's directory fetcher.
//! Every program the fixture starts is its own spawn and has ended before the fixture returns.

use crate::support::{sot_capsule_exe, sotd_program};
use sot_updater::fetch::hash::sha256_bytes;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The release the fixture offers: newer than any build the suite runs.
pub const VERSION: &str = "9.9.9";

pub struct Release {
    /// The copy of the daemon inside the install-shaped prefix, the binary an automatic update case starts.
    pub daemon: PathBuf,
    /// The value of `SOT_UPDATE_FETCHER` that serves the release folder.
    pub fetcher: String,
    /// The install's updates root (`<prefix>/updates`), where a stage, a prepared worktree's record and the pointer land.
    pub updates: PathBuf,
}

fn run(program: &str, dir: &Path, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("run {program}: {e}"));
    assert!(
        out.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut full = vec![
        "-c",
        "user.name=fixture",
        "-c",
        "user.email=fixture@example.invalid",
        "-c",
        "init.defaultBranch=main",
    ];
    full.extend_from_slice(args);
    run("git", dir, &full)
}

/// Build the fixture under `root`.
pub fn build(root: &Path) -> Release {
    let target =
        sot_updater::platform::this_platform().expect("the test host is in the release matrix");
    let tag = format!("v{VERSION}");

    let upstream = root.join("upstream");
    std::fs::create_dir_all(&upstream).unwrap();
    git(&upstream, &["init", "-q"]);
    std::fs::write(upstream.join("README"), b"the release's source\n").unwrap();
    git(&upstream, &["add", "README"]);
    git(&upstream, &["commit", "-q", "-m", "the release"]);
    git(&upstream, &["tag", &tag]);
    let commit = git(&upstream, &["rev-parse", &format!("{tag}^{{commit}}")]);

    let prefix = root.join("prefix");
    let bin = prefix.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let daemon = bin.join("sotd");
    std::fs::copy(sotd_program(), &daemon).expect("copy the built daemon into the prefix");
    std::fs::copy(sot_capsule_exe(), bin.join("sot-capsule")).expect("copy the capsule beside it");
    std::fs::write(
        prefix.join("install.json"),
        format!(
            "{{\"schema\":1,\"prefix\":{:?},\"service\":\"none\",\"daemon\":false,\"frontend\":false}}",
            prefix.display().to_string()
        ),
    )
    .unwrap();
    let repo = prefix.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(
        &repo,
        &[
            "clone",
            "-q",
            &upstream.display().to_string(),
            &repo.join("base").display().to_string(),
        ],
    );

    let release = root.join("release");
    let top = format!("sot-{VERSION}-{target}");
    let build = root.join("build").join(&top);
    std::fs::create_dir_all(&build).unwrap();
    for (name, body) in [
        ("sot", "fe-binary"),
        ("sotd", "be-binary"),
        ("sot-capsule", "capsule-binary"),
        ("sotd.service", "unit"),
    ] {
        std::fs::write(build.join(name), body).unwrap();
    }
    std::fs::create_dir_all(&release).unwrap();
    let asset = sot_updater::platform::platform_asset(VERSION).expect("an asset for this platform");
    run(
        "tar",
        root,
        &[
            "-czf",
            &release.join(&asset).display().to_string(),
            "-C",
            &root.join("build").display().to_string(),
            &top,
        ],
    );
    std::fs::write(release.join("COMMIT"), format!("{commit}\n")).unwrap();
    let digest = |name: &str| sha256_bytes(&std::fs::read(release.join(name)).unwrap());
    std::fs::write(
        release.join("SHA256SUMS"),
        format!(
            "{}  {asset}\n{}  COMMIT\n",
            digest(&asset),
            digest("COMMIT")
        ),
    )
    .unwrap();

    Release {
        daemon,
        fetcher: format!("dir:{}", release.display()),
        updates: prefix.join("updates"),
    }
}
