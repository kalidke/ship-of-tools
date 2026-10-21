//! Release-fetch backends.
//!
//! `Curl` is the default and the public-install path: plain HTTPS to
//! github.com via the `curl` binary (a hard install requirement on every
//! platform — ships with Windows 10+ and macOS). Discovery is two requests,
//! not one: `list_releases` reads the public `GET
//! /repos/<repo>/releases?per_page=20` REST listing (tag + prerelease flag
//! for recent releases, no auth needed on a public repo), [`select_target`]
//! picks the tag implied by the installed version's channel (ADR 0030 §4
//! amendment 2026-09-08), then `latest` downloads that one tag's
//! `SHA256SUMS` exactly like any other named release file (see
//! `sums::discover`).
//!
//! `Gh` shells out to the authenticated `gh` CLI — needed only for private
//! forks (`SOT_UPDATE_FETCHER=gh`). `Dir` serves "downloads" from a local
//! directory: the test backend, and a manual sideload path for air-gapped
//! machines (`SOT_UPDATE_FETCHER=dir:/path`).

pub mod archive;
pub mod hash;
pub mod sums;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Spawner;
use sot_platform::host::HeldDir;

use anyhow::{anyhow, bail, Context, Result};

use crate::identity::{validate_repo, validate_tag};
use crate::select::select_target;

/// What a latest-release query yields: the `SHA256SUMS` document (the
/// discovery source of truth) plus the tag when the backend happens to know it
/// authoritatively (every backend does, now that discovery starts from a
/// release listing rather than the `/latest` alias). When both exist,
/// callers cross-check them.
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
        match std::env::var("SOT_UPDATE_FETCHER")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("gh") => Fetcher::Gh,
            Some(s) if s.starts_with("dir:") => Fetcher::Dir(PathBuf::from(&s[4..])),
            None | Some("") | Some("curl") => Fetcher::Curl,
            Some(other) => {
                tracing::warn!(value = %other, "unknown SOT_UPDATE_FETCHER; defaulting to curl");
                Fetcher::Curl
            }
        }
    }

    /// List recent releases as `(tag, prerelease)` pairs — the raw material
    /// [`select_target`] chooses from. Most-recent-first, unfiltered (a
    /// draft is dropped here since it has no public download URL at all;
    /// everything else, including prereleases, is left for the selection
    /// function to filter by channel).
    pub(crate) async fn list_releases(
        &self,
        spawner: &dyn Spawner,
        repo: &str,
    ) -> Result<Vec<(String, bool)>> {
        validate_repo(repo)?;
        match self {
            Fetcher::Curl => {
                let url = format!("https://api.github.com/repos/{repo}/releases?per_page=20");
                // Short budget: this runs inline in the `update.check` op —
                // a slow check must not wedge the daemon connection.
                let bytes = curl_fetch(spawner, &url, None, Duration::from_secs(30)).await?;
                parse_release_list(&bytes)
            }
            Fetcher::Gh => {
                let stdout = run_cmd(
                    spawner,
                    "gh",
                    &["api", &format!("repos/{repo}/releases?per_page=20")],
                    Duration::from_secs(30),
                )
                .await?;
                parse_release_list(&stdout)
            }
            Fetcher::Dir(dir) => {
                // A sideload/test directory holds one release's files, not a
                // listing. An optional `releases.json` (same shape as the
                // GitHub API array) lets a test exercise multi-release
                // selection; otherwise derive a single stable-channel entry
                // from the local SHA256SUMS, exactly what every prior test
                // fixture already provides.
                if let Ok(bytes) = tokio::fs::read(dir.join("releases.json")).await {
                    return parse_release_list(&bytes);
                }
                let sums_text = tokio::fs::read_to_string(dir.join("SHA256SUMS"))
                    .await
                    .with_context(|| format!("reading {}/SHA256SUMS", dir.display()))?;
                let entries = sums::parse_sums(&sums_text)?;
                let version = entries
                    .iter()
                    .find_map(|e| crate::platform::parse_asset_name(&e.name).map(|(v, _)| v))
                    .ok_or_else(|| {
                        anyhow!(
                            "no recognizable sot-* asset in {}/SHA256SUMS",
                            dir.display()
                        )
                    })?;
                Ok(vec![(format!("v{version}"), false)])
            }
        }
    }

    /// Resolve the tag [`select_target`] wants (via [`Fetcher::list_releases`])
    /// and fetch that release's `SHA256SUMS`. `Ok(None)` means the check
    /// succeeded but nothing in `current_version`'s channel is newer — a
    /// normal outcome, not an error.
    pub(crate) async fn latest(
        &self,
        spawner: &dyn Spawner,
        repo: &str,
        current_version: &str,
    ) -> Result<Option<LatestRelease>> {
        validate_repo(repo)?;
        remove_ended_scratch().await;
        let releases = self.list_releases(spawner, repo).await?;
        let Some(tag) = select_target(current_version, &releases) else {
            return Ok(None);
        };
        validate_tag(&tag)?;
        let dir = scratch_dir().await?;
        let sums_path = dir.path().join("SHA256SUMS");
        let res = self
            .download(spawner, repo, &tag, "SHA256SUMS", &sums_path)
            .await;
        let text = match res {
            Ok(()) => tokio::fs::read_to_string(&sums_path)
                .await
                .context("reading downloaded SHA256SUMS"),
            Err(e) => Err(e),
        };
        let _ = tokio::task::spawn_blocking(move || dir.remove()).await;
        Ok(Some(LatestRelease {
            tag: Some(tag),
            sums_text: text?,
        }))
    }

    /// Download one named release file for an explicit tag into `dest` (a file
    /// path). Callers validate the identity; this validates the raw strings
    /// again before building URLs/paths (defense in depth).
    pub(crate) async fn download(
        &self,
        spawner: &dyn Spawner,
        repo: &str,
        tag: &str,
        name: &str,
        dest: &Path,
    ) -> Result<()> {
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
                curl_fetch(spawner, &url, Some(dest), Duration::from_secs(900)).await?;
                Ok(())
            }
            Fetcher::Gh => {
                let dir = dest
                    .parent()
                    .ok_or_else(|| anyhow!("download dest has no parent"))?;
                run_cmd(
                    spawner,
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

/// Parse a GitHub releases-list JSON array (the shape both `GET
/// /repos/<repo>/releases` and `gh api repos/<repo>/releases` return) into
/// `(tag, prerelease)` pairs. An entry missing either field, or a draft
/// (no public download URL to pin), is dropped rather than guessed at —
/// [`select_target`] already treats a missing candidate as "no update", the
/// safe default.
fn parse_release_list(bytes: &[u8]) -> Result<Vec<(String, bool)>> {
    let v: serde_json::Value = serde_json::from_slice(bytes).context("parsing releases JSON")?;
    let arr = v
        .as_array()
        .ok_or_else(|| anyhow!("releases JSON is not an array"))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        if item.get("draft").and_then(|d| d.as_bool()).unwrap_or(false) {
            continue;
        }
        let (Some(tag), Some(prerelease)) = (
            item.get("tag_name").and_then(|t| t.as_str()),
            item.get("prerelease").and_then(|p| p.as_bool()),
        ) else {
            continue;
        };
        out.push((tag.to_string(), prerelease));
    }
    Ok(out)
}

/// Fetch a URL with curl, HTTPS-only end to end (`--proto =https
/// --proto-redir =https`), bounded redirects and time. With `output`, curl
/// streams straight to that file and the returned Vec is empty; without it,
/// the body is returned (small documents only).
async fn curl_fetch(
    spawner: &dyn Spawner,
    url: &str,
    output: Option<&Path>,
    max_time: Duration,
) -> Result<Vec<u8>> {
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
    run_cmd(spawner, "curl", &args, max_time + Duration::from_secs(30)).await
}

/// Run a command with a timeout, killing the child if the timeout fires
/// (kill_on_drop). A missing binary maps to a clear error; a nonzero exit
/// surfaces the first stderr line.
async fn run_cmd(
    spawner: &dyn Spawner,
    bin: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, spawner.output(&mut cmd)).await {
        Err(_) => bail!("{bin} timed out after {}s", timeout.as_secs()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => bail!("{bin} not found"),
        Ok(Err(e)) => return Err(e).with_context(|| format!("spawning {bin}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        bail!(
            "{bin} failed: {}",
            if first.is_empty() {
                "unknown error"
            } else {
                first
            }
        );
    }
    Ok(out.stdout)
}

/// How a release check's scratch folder is named in the system's temporary folder: a random part follows.
const SCRATCH: &str = "sot-updater-";

/// A scratch folder of its own for one check's download, in the system's temporary folder (fetch-internal only: real
/// staging folders live under the updates root, see `stage`). It is a held folder (`sot_platform::host::HeldDir`): this
/// process holds it until its check removes it, so two checks in one process (the daemon's periodic check and an
/// `update.check` op can overlap) each have their own, and neither the other nor another process removes it. One left
/// by a check whose process ended first (a crash, a kill mid-download) goes at a later check ([`remove_ended_scratch`]).
async fn scratch_dir() -> Result<HeldDir> {
    let made = tokio::task::spawn_blocking(|| HeldDir::create(&std::env::temp_dir(), SCRATCH)).await?;
    made.context("making the release check's scratch folder")
}

/// Removes the scratch folders of checks whose process has ended, at the start of every check, in whichever process
/// runs it (the daemon's or the window's); never one whose process still runs, whatever computer or pid namespace it
/// runs in, since each folder's lock is held until its process ends.
async fn remove_ended_scratch() {
    let removed = tokio::task::spawn_blocking(|| sot_platform::host::remove_ended(&std::env::temp_dir(), SCRATCH))
        .await
        .unwrap_or(0);
    if removed > 0 {
        tracing::info!(removed, "scratch folders of ended release checks removed");
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
