//! Archive entry validation and bounded extraction.
//!
//! The release archive shape is exactly known — one `sot-<ver>-<target>/`
//! directory holding an allowlisted set of files — so validation is an
//! allowlist, not a denylist (Codex review, MUST-FIX 11). Names are checked
//! BEFORE extraction (a hostile `../` or absolute entry never touches the
//! tree), and the extracted tree is re-walked afterwards to reject anything
//! the lister missed (symlinks, stray files). Extraction runs with a timeout
//! and kill-on-drop.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

const EXTRACT_TIMEOUT: Duration = Duration::from_secs(300);

/// A sane single filename component: non-empty, bounded, no path separators
/// or traversal or control characters, doesn't start with a dot. Release
/// archives may grow NEW files over time and already-deployed updaters must
/// not refuse them (auto-update is the only channel that ships fixes), so
/// membership in a fixed list is deliberately NOT required — the security
/// property is path/symlink/shape control plus the per-platform REQUIRED
/// set, not filename enumeration.
fn sane_component(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Validate one archive entry name against the expected top-level dir
/// (`sot-<version>-<target>`). Accepts `top/`, `top` and `top/<sane-file>`;
/// rejects everything else — absolute paths, `..`, backslashes, deeper
/// nesting, hostile names.
pub fn validate_entry_name(entry: &str, top: &str) -> Result<()> {
    if entry.contains('\\') || entry.contains('\u{0}') {
        bail!("archive entry {entry:?} contains forbidden characters");
    }
    if entry.starts_with('/') || entry.starts_with("./") || entry.contains("..") {
        bail!("archive entry {entry:?} is not a plain relative path");
    }
    let trimmed = entry.strip_suffix('/').unwrap_or(entry);
    if trimmed == top {
        return Ok(()); // the top-level dir itself
    }
    let Some(rest) = trimmed.strip_prefix(&format!("{top}/")) else {
        bail!("archive entry {entry:?} is outside the expected {top}/ dir");
    };
    if sane_component(rest) {
        Ok(())
    } else {
        bail!("archive entry {entry:?} is not a sane release filename");
    }
}

/// List entry names for a `.tar.gz` (via `tar -tzf`) or `.zip` (via
/// `zipinfo -1` / PowerShell on Windows).
pub async fn list_entries(archive: &Path) -> Result<Vec<String>> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let stdout = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        run(
            tokio::process::Command::new("tar").args(["-tzf", &archive.to_string_lossy()]),
            "tar -t",
        )
        .await?
    } else if name.ends_with(".zip") {
        #[cfg(windows)]
        {
            // -EncodedCommand is overkill; entry names come from ZipFile, not
            // string-interpolated shell.
            run(
                tokio::process::Command::new("powershell").args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "Add-Type -AssemblyName System.IO.Compression.FileSystem; \
                         [IO.Compression.ZipFile]::OpenRead('{}').Entries | ForEach-Object {{ $_.FullName }}",
                        archive.display().to_string().replace('\'', "''")
                    ),
                ]),
                "zip list",
            )
            .await?
        }
        #[cfg(not(windows))]
        {
            run(
                tokio::process::Command::new("zipinfo").args(["-1", &archive.to_string_lossy()]),
                "zipinfo",
            )
            .await?
        }
    } else {
        bail!("don't know how to list {name}");
    };
    Ok(String::from_utf8_lossy(&stdout)
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Extract a verified archive into `dest`: entry names are validated against
/// `top` first, extraction is bounded, and the resulting tree is re-validated
/// (no symlinks, only allowlisted files under `dest/top/`).
pub async fn extract_validated(archive: &Path, dest: &Path, top: &str) -> Result<()> {
    let entries = list_entries(archive).await?;
    if entries.is_empty() {
        bail!("archive lists no entries");
    }
    for e in &entries {
        validate_entry_name(e, top)?;
    }

    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        run(
            tokio::process::Command::new("tar").args([
                "-xzf",
                &archive.to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ]),
            "tar extract",
        )
        .await?;
    } else if name.ends_with(".zip") {
        #[cfg(windows)]
        {
            run(
                tokio::process::Command::new("powershell").args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        // ErrorActionPreference=Stop: a non-terminating
                        // extractor error must exit nonzero, never commit a
                        // partial tree as staged.
                        "$ErrorActionPreference='Stop'; Expand-Archive -Force -ErrorAction Stop -LiteralPath '{}' -DestinationPath '{}'",
                        archive.display().to_string().replace('\'', "''"),
                        dest.display().to_string().replace('\'', "''")
                    ),
                ]),
                "Expand-Archive",
            )
            .await?;
        }
        #[cfg(not(windows))]
        {
            run(
                tokio::process::Command::new("unzip").args([
                    "-o",
                    &archive.to_string_lossy(),
                    "-d",
                    &dest.to_string_lossy(),
                ]),
                "unzip",
            )
            .await?;
        }
    } else {
        bail!("don't know how to unpack {name}");
    }

    validate_tree(&dest.join(top), top).await
}

/// Post-extraction sweep of `dir` (= `dest/top`): regular files from the
/// allowlist only, no symlinks, no subdirectories — and the platform's
/// REQUIRED binaries must actually be present ("not empty" is not enough: a
/// non-terminating extractor error must never let a partial stage read as
/// complete).
async fn validate_tree(dir: &Path, top: &str) -> Result<()> {
    let mut rd = tokio::fs::read_dir(dir)
        .await
        .with_context(|| format!("extracted dir {} missing", dir.display()))?;
    let mut seen: Vec<String> = Vec::new();
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        let meta = tokio::fs::symlink_metadata(entry.path()).await?;
        if meta.file_type().is_symlink() {
            bail!("extracted {top}/{name} is a symlink — rejecting");
        }
        if !meta.is_file() {
            bail!("extracted {top}/{name} is not a regular file — rejecting");
        }
        if !sane_component(&name) {
            bail!("extracted {top}/{name} is not a sane release filename");
        }
        if meta.len() == 0 {
            bail!("extracted {top}/{name} is empty — rejecting partial extraction");
        }
        seen.push(name);
    }
    let required: &[&str] = if top.ends_with("windows-x86_64") {
        &["sot.exe", "sotd.exe"]
    } else {
        &["sot", "sotd"]
    };
    for want in required {
        if !seen.iter().any(|s| s == want) {
            bail!("extracted {top}/ is missing required file {want}");
        }
    }
    Ok(())
}

async fn run(cmd: &mut tokio::process::Command, what: &str) -> Result<Vec<u8>> {
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let out = match tokio::time::timeout(EXTRACT_TIMEOUT, cmd.output()).await {
        Err(_) => bail!("{what} timed out after {}s", EXTRACT_TIMEOUT.as_secs()),
        Ok(r) => r.with_context(|| format!("spawning {what}"))?,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("{what} exit {:?}: {}", out.status.code(), stderr.trim());
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        tokio::fs::write(stage.join("sotd.service"), b"unit").await.unwrap();
        let archive = base.join(format!("{TOP}.tar.gz"));
        let st = std::process::Command::new("tar")
            .args([
                "-czf",
                &archive.to_string_lossy(),
                "-C",
                &base.join("build").to_string_lossy(),
                TOP,
            ])
            .status()
            .unwrap();
        assert!(st.success());

        let entries = list_entries(&archive).await.unwrap();
        assert!(entries.iter().any(|e| e.trim_end_matches('/') == TOP));

        let dest = base.join("out");
        tokio::fs::create_dir_all(&dest).await.unwrap();
        extract_validated(&archive, &dest, TOP).await.unwrap();
        assert_eq!(
            tokio::fs::read(dest.join(TOP).join("sot")).await.unwrap(),
            b"fe"
        );

        // A missing required binary fails the post-extraction sweep.
        tokio::fs::remove_file(base.join("build").join(TOP).join("sotd"))
            .await
            .unwrap();
        let bad = base.join("bad.tar.gz");
        let st = std::process::Command::new("tar")
            .args([
                "-czf",
                &bad.to_string_lossy(),
                "-C",
                &base.join("build").to_string_lossy(),
                TOP,
            ])
            .status()
            .unwrap();
        assert!(st.success());
        let dest2 = base.join("out2");
        tokio::fs::create_dir_all(&dest2).await.unwrap();
        let err = extract_validated(&bad, &dest2, TOP).await.unwrap_err();
        assert!(err.to_string().contains("missing required file"), "{err}");

        tokio::fs::remove_dir_all(&base).await.unwrap();
    }
}
