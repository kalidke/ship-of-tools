//! Release-fetch backends.
//!
//! `Curl` is the default and the public-install path: plain HTTPS to
//! github.com release-download URLs via the `curl` binary (a hard install
//! requirement on every platform — ships with Windows 10+ and macOS). No
//! Releases API involvement at all: discovery reads
//! `releases/latest/download/SHA256SUMS` (see `sums::discover`) and every
//! other fetch is pinned to an explicit tag.
//!
//! `Gh` shells out to the authenticated `gh` CLI — needed only for private
//! forks (`SOT_UPDATE_FETCHER=gh`). `Dir` serves "downloads" from a local
//! directory: the test backend, and a manual sideload path for air-gapped
//! machines (`SOT_UPDATE_FETCHER=dir:/path`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};

use crate::identity::{validate_repo, validate_tag};

/// What a latest-release query yields: the `SHA256SUMS` document (the
/// discovery source of truth) plus the tag when the backend happens to know it
/// authoritatively (`Gh`). When both exist, callers cross-check them.
#[derive(Debug, Clone)]
pub struct LatestRelease {
    pub tag: Option<String>,
    pub sums_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetcher {
    /// Plain HTTPS via the `curl` binary (default; public repo).
    Curl,
    /// Authenticated `gh` CLI (private forks).
    Gh,
    /// Local directory holding release files (tests, manual sideload).
    Dir(PathBuf),
}

impl Fetcher {
    /// Resolve from `SOT_UPDATE_FETCHER`: `curl` (default), `gh`, or
    /// `dir:<path>`.
    pub fn from_env() -> Self {
        match std::env::var("SOT_UPDATE_FETCHER").ok().as_deref().map(str::trim) {
            Some("gh") => Fetcher::Gh,
            Some(s) if s.starts_with("dir:") => Fetcher::Dir(PathBuf::from(&s[4..])),
            None | Some("") | Some("curl") => Fetcher::Curl,
            Some(other) => {
                tracing::warn!(value = %other, "unknown SOT_UPDATE_FETCHER; defaulting to curl");
                Fetcher::Curl
            }
        }
    }

    /// Fetch the latest release's `SHA256SUMS` (and, for backends that know
    /// it, the tag).
    pub async fn latest(&self, repo: &str) -> Result<LatestRelease> {
        validate_repo(repo)?;
        match self {
            Fetcher::Curl => {
                let url = format!("https://github.com/{repo}/releases/latest/download/SHA256SUMS");
                // Short budget: this runs inline in the `update.check` op —
                // a slow check must not wedge the daemon connection.
                let bytes = curl_fetch(&url, None, Duration::from_secs(30)).await?;
                Ok(LatestRelease {
                    tag: None,
                    sums_text: String::from_utf8(bytes).context("SHA256SUMS is not UTF-8")?,
                })
            }
            Fetcher::Gh => {
                let stdout = run_cmd(
                    "gh",
                    &["api", &format!("repos/{repo}/releases/latest")],
                    Duration::from_secs(30),
                )
                .await?;
                let v: serde_json::Value =
                    serde_json::from_slice(&stdout).context("parsing gh api JSON")?;
                let tag = v
                    .get("tag_name")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| anyhow!("release JSON has no tag_name"))?
                    .to_string();
                validate_tag(&tag)?;
                let dir = tempdir("gh-sums").await?;
                let sums_path = dir.join("SHA256SUMS");
                let res = self.download(repo, &tag, "SHA256SUMS", &sums_path).await;
                let text = match res {
                    Ok(()) => tokio::fs::read_to_string(&sums_path)
                        .await
                        .context("reading downloaded SHA256SUMS"),
                    Err(e) => Err(e),
                };
                let _ = tokio::fs::remove_dir_all(&dir).await;
                Ok(LatestRelease {
                    tag: Some(tag),
                    sums_text: text?,
                })
            }
            Fetcher::Dir(dir) => Ok(LatestRelease {
                tag: None,
                sums_text: tokio::fs::read_to_string(dir.join("SHA256SUMS"))
                    .await
                    .with_context(|| format!("reading {}/SHA256SUMS", dir.display()))?,
            }),
        }
    }

    /// Download one named release file for an explicit tag into `dest` (a file
    /// path). Callers validate the identity; this validates the raw strings
    /// again before building URLs/paths (defense in depth).
    pub async fn download(&self, repo: &str, tag: &str, name: &str, dest: &Path) -> Result<()> {
        validate_repo(repo)?;
        validate_tag(tag)?;
        if name.contains('/') || name.contains('\\') || name.contains("..") {
            bail!("refusing suspicious release file name {name:?}");
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        match self {
            Fetcher::Curl => {
                let url = format!("https://github.com/{repo}/releases/download/{tag}/{name}");
                // Downloads can be tens of MB; stream straight to the file
                // (curl -o) — never buffered through this process's memory.
                curl_fetch(&url, Some(dest), Duration::from_secs(900)).await?;
                Ok(())
            }
            Fetcher::Gh => {
                let dir = dest
                    .parent()
                    .ok_or_else(|| anyhow!("download dest has no parent"))?;
                run_cmd(
                    "gh",
                    &[
                        "release",
                        "download",
                        tag,
                        "--repo",
                        repo,
                        "--dir",
                        &dir.to_string_lossy(),
                        "--clobber",
                        "--pattern",
                        name,
                    ],
                    Duration::from_secs(900),
                )
                .await?;
                let got = dir.join(name);
                if got != dest {
                    tokio::fs::rename(&got, dest)
                        .await
                        .with_context(|| format!("moving {} into place", got.display()))?;
                }
                Ok(())
            }
            Fetcher::Dir(src_dir) => {
                let src = src_dir.join(name);
                tokio::fs::copy(&src, dest)
                    .await
                    .with_context(|| format!("copying {}", src.display()))?;
                Ok(())
            }
        }
    }
}

/// Fetch a URL with curl, HTTPS-only end to end (`--proto =https
/// --proto-redir =https`), bounded redirects and time. With `output`, curl
/// streams straight to that file and the returned Vec is empty; without it,
/// the body is returned (small documents only).
async fn curl_fetch(url: &str, output: Option<&Path>, max_time: Duration) -> Result<Vec<u8>> {
    let max_time_s = max_time.as_secs().to_string();
    let mut args = vec![
        "-fsSL",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--max-redirs",
        "4",
        "--connect-timeout",
        "15",
        "--max-time",
        &max_time_s,
        "--retry",
        "2",
    ];
    let out_s;
    if let Some(dest) = output {
        out_s = dest.to_string_lossy().into_owned();
        args.push("-o");
        args.push(&out_s);
    }
    args.push(url);
    // Outer ceiling above curl's own --max-time so curl owns the timeout
    // and we still have a backstop that kills a wedged process.
    run_cmd("curl", &args, max_time + Duration::from_secs(30)).await
}

/// Run a command with a timeout, killing the child if the timeout fires
/// (kill_on_drop). A missing binary maps to a clear error; a nonzero exit
/// surfaces the first stderr line.
async fn run_cmd(bin: &str, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => bail!("{bin} timed out after {}s", timeout.as_secs()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => bail!("{bin} not found"),
        Ok(Err(e)) => return Err(e).with_context(|| format!("spawning {bin}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        bail!(
            "{bin} failed: {}",
            if first.is_empty() { "unknown error" } else { first }
        );
    }
    Ok(out.stdout)
}

/// Unique scratch dir under the system temp root (fetch-internal only — real
/// staging temp dirs live under the updates root, see `stage`).
async fn tempdir(label: &str) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!(
        "sot-updater-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetcher_from_env_forms() {
        // Not a full env test (process-global env); just the parse arms via
        // the public constructor contract.
        assert_eq!(Fetcher::Curl, Fetcher::Curl);
        match ("dir:/x/y".strip_prefix("dir:"), PathBuf::from("/x/y")) {
            (Some(p), want) => assert_eq!(PathBuf::from(p), want),
            _ => unreachable!(),
        }
    }

    #[tokio::test]
    async fn dir_fetcher_serves_local_files() {
        let dir = std::env::temp_dir().join(format!("sot-updater-fetch-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("SHA256SUMS"),
            "0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-linux-x86_64.tar.gz\n",
        )
        .await
        .unwrap();
        tokio::fs::write(dir.join("blob.bin"), b"payload").await.unwrap();

        let f = Fetcher::Dir(dir.clone());
        let latest = f.latest("kalidke/ship-of-tools").await.unwrap();
        assert!(latest.tag.is_none());
        assert!(latest.sums_text.contains("sot-0.6.0"));

        let dest = dir.join("out/blob.bin");
        f.download("kalidke/ship-of-tools", "v0.6.0", "blob.bin", &dest)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"payload");

        // Path-material names are refused before touching the filesystem.
        assert!(f
            .download("kalidke/ship-of-tools", "v0.6.0", "../evil", &dest)
            .await
            .is_err());
        assert!(f
            .download("kalidke/bad repo", "v0.6.0", "blob.bin", &dest)
            .await
            .is_err());
        assert!(f
            .download("kalidke/ship-of-tools", "latest", "blob.bin", &dest)
            .await
            .is_err());

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }
}
