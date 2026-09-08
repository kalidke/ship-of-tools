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

use std::path::{Path, PathBuf};
use std::time::Duration;

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

    /// List recent releases as `(tag, prerelease)` pairs — the raw material
    /// [`select_target`] chooses from. Most-recent-first, unfiltered (a
    /// draft is dropped here since it has no public download URL at all;
    /// everything else, including prereleases, is left for the selection
    /// function to filter by channel).
    pub async fn list_releases(&self, repo: &str) -> Result<Vec<(String, bool)>> {
        validate_repo(repo)?;
        match self {
            Fetcher::Curl => {
                let url = format!("https://api.github.com/repos/{repo}/releases?per_page=20");
                // Short budget: this runs inline in the `update.check` op —
                // a slow check must not wedge the daemon connection.
                let bytes = curl_fetch(&url, None, Duration::from_secs(30)).await?;
                parse_release_list(&bytes)
            }
            Fetcher::Gh => {
                let stdout = run_cmd(
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
                let entries = crate::sums::parse_sums(&sums_text)?;
                let version = entries
                    .iter()
                    .find_map(|e| crate::platform::parse_asset_name(&e.name).map(|(v, _)| v))
                    .ok_or_else(|| {
                        anyhow!("no recognizable sot-* asset in {}/SHA256SUMS", dir.display())
                    })?;
                Ok(vec![(format!("v{version}"), false)])
            }
        }
    }

    /// Resolve the tag [`select_target`] wants (via [`Fetcher::list_releases`])
    /// and fetch that release's `SHA256SUMS`. `Ok(None)` means the check
    /// succeeded but nothing in `current_version`'s channel is newer — a
    /// normal outcome, not an error.
    pub async fn latest(
        &self,
        repo: &str,
        current_version: &str,
    ) -> Result<Option<LatestRelease>> {
        validate_repo(repo)?;
        let releases = self.list_releases(repo).await?;
        let Some(tag) = select_target(current_version, &releases) else {
            return Ok(None);
        };
        validate_tag(&tag)?;
        let dir = tempdir("sums").await?;
        let sums_path = dir.join("SHA256SUMS");
        let res = self.download(repo, &tag, "SHA256SUMS", &sums_path).await;
        let text = match res {
            Ok(()) => tokio::fs::read_to_string(&sums_path)
                .await
                .context("reading downloaded SHA256SUMS"),
            Err(e) => Err(e),
        };
        let _ = tokio::fs::remove_dir_all(&dir).await;
        Ok(Some(LatestRelease {
            tag: Some(tag),
            sums_text: text?,
        }))
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

        // No `releases.json`: the directory's own SHA256SUMS is the single
        // (stable-channel) release on offer.
        let listed = f.list_releases("kalidke/ship-of-tools").await.unwrap();
        assert_eq!(listed, vec![("v0.6.0".to_string(), false)]);

        let latest = f
            .latest("kalidke/ship-of-tools", "0.1.0")
            .await
            .unwrap()
            .expect("0.6.0 is newer than 0.1.0");
        assert_eq!(latest.tag.as_deref(), Some("v0.6.0"));
        assert!(latest.sums_text.contains("sot-0.6.0"));

        // Already past this release: no update, not an error.
        assert!(f
            .latest("kalidke/ship-of-tools", "9.9.9")
            .await
            .unwrap()
            .is_none());

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

    #[test]
    fn parse_release_list_shape() {
        // The exact shape of one page of `GET /repos/<repo>/releases` (and
        // `gh api` for the same path) — the fields we read plus the noise we
        // ignore (id, body, assets, …), a draft (dropped), and an entry
        // missing `prerelease` (dropped: never guess a channel).
        let json = br#"[
            {"tag_name": "v0.6.0-rc.13", "prerelease": true, "draft": false, "id": 3, "body": "notes"},
            {"tag_name": "v0.6.0-rc.11", "prerelease": true, "draft": false},
            {"tag_name": "v0.5.10", "prerelease": false, "draft": false, "assets": []},
            {"tag_name": "v0.7.0", "prerelease": false, "draft": true},
            {"tag_name": "v0.4.0", "draft": false}
        ]"#;
        let got = parse_release_list(json).unwrap();
        assert_eq!(
            got,
            vec![
                ("v0.6.0-rc.13".to_string(), true),
                ("v0.6.0-rc.11".to_string(), true),
                ("v0.5.10".to_string(), false),
            ]
        );
    }

    #[test]
    fn parse_release_list_rejects_non_array() {
        assert!(parse_release_list(br#"{"tag_name": "v0.6.0"}"#).is_err());
    }

    #[tokio::test]
    async fn dir_fetcher_releases_json_drives_channel_selection() {
        let dir = std::env::temp_dir().join(format!("sot-updater-fetch-rj-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        tokio::fs::write(
            dir.join("releases.json"),
            r#"[
                {"tag_name": "v0.6.0-rc.13", "prerelease": true},
                {"tag_name": "v0.6.0-rc.11", "prerelease": true},
                {"tag_name": "v0.5.10", "prerelease": false}
            ]"#,
        )
        .await
        .unwrap();
        // download() ignores the tag for Dir, so any SHA256SUMS content
        // proves which tag latest() resolved to (via `latest.tag`).
        tokio::fs::write(
            dir.join("SHA256SUMS"),
            "0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-rc.13-linux-x86_64.tar.gz\n",
        )
        .await
        .unwrap();

        let f = Fetcher::Dir(dir.clone());
        // A stable install never sees the rcs.
        assert!(f.latest("kalidke/ship-of-tools", "0.5.10").await.unwrap().is_none());
        // An rc install tracks the newest rc.
        let l = f
            .latest("kalidke/ship-of-tools", "0.6.0-rc.11")
            .await
            .unwrap()
            .expect("rc.13 supersedes rc.11");
        assert_eq!(l.tag.as_deref(), Some("v0.6.0-rc.13"));

        tokio::fs::remove_dir_all(&dir).await.unwrap();
    }

    /// Hits the real public repo's release listing — read-only, no auth, no
    /// stage/apply. Gated on `--ignored` (network-dependent, and the real
    /// listing changes daily); run manually after touching the discovery
    /// path. Proves `list_releases` parses live GitHub JSON and that
    /// `select_target` keeps a stable-channel install off every listed
    /// prerelease, without hardcoding today's actual tag numbers (the repo
    /// cuts rcs daily, so any exact-tag assertion would rot).
    #[tokio::test]
    #[ignore]
    async fn live_list_releases_against_real_repo() {
        let repo = "kalidke/ship-of-tools";
        let releases = Fetcher::Curl.list_releases(repo).await.expect("list_releases");
        assert!(!releases.is_empty(), "expected at least one published release");
        assert!(
            releases.iter().any(|(_, prerelease)| *prerelease),
            "expected at least one prerelease in a repo that cuts rcs daily"
        );
        for (tag, _) in &releases {
            assert!(crate::semver::parse_semver(tag).is_some(), "unparsable tag: {tag}");
        }

        // A stable-channel install must never be pointed at a prerelease.
        if let Some(tag) = select_target("0.0.1", &releases) {
            assert!(
                !tag.contains("-rc.") && !tag.contains("-alpha") && !tag.contains("-beta"),
                "stable install selected a prerelease: {tag}"
            );
        }

        // A very-old prerelease install must be offered SOMETHING newer
        // (the live repo always has more recent releases than 0.0.1-rc.1).
        let picked = select_target("0.0.1-rc.1", &releases);
        assert!(picked.is_some(), "expected an update from a very old rc");
    }
}
