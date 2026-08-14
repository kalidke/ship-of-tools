//! Transactional version preparation (ADR 0030 Phase C2).
//!
//! Since the clone-based-install amendment, a product version is binaries
//! **plus** the tag-pinned repo checkout **plus** instantiated Julia
//! environments. Swapping only binaries ships a skewed install, and running
//! `git checkout` over the live `repo/current` tree mutates what the running
//! daemon is actively resolving resources from. So preparation happens OFF
//! the live tree, entirely at stage time:
//!
//! ```text
//! <prefix>/repo/base            bare-ish blobless clone (fetch target)
//! <prefix>/repo/versions/<tag>  detached git worktree at the tag's commit
//! <prefix>/repo/current         the LIVE tree — never touched here
//! ```
//!
//! Apply (Phase C3) is then a fast, offline pointer flip: `current` →
//! `versions/<tag>`, binaries from the staged ready dir, daemon/FE restart.
//! Rollback is the same flip to the previous version dir.
//!
//! Everything here is best-effort-resumable: a crashed prepare leaves a
//! versions dir without a `prepared.json`, and the next run rebuilds it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::identity::ReleaseIdentity;

/// Where prepared-state is recorded: `<updates-root>/<tag>/prepared.json`,
/// next to (but separate from) the immutable ready manifest.
pub const PREPARED_MANIFEST: &str = "prepared.json";

/// Ceilings for the long-running steps. Julia instantiate compiles real code;
/// be generous — prepare runs in the background while the old version serves.
const GIT_TIMEOUT: Duration = Duration::from_secs(1800);
const JULIA_TIMEOUT: Duration = Duration::from_secs(3600);
const NPM_TIMEOUT: Duration = Duration::from_secs(900);

/// What to prepare and where.
#[derive(Debug, Clone)]
pub struct PrepareSpec {
    pub identity: ReleaseIdentity,
    /// `<prefix>/repo` — holds `base`, `versions/`, and the live `current`.
    pub repo_dir: PathBuf,
    /// `<updates-root>/<tag>` — the completed stage dir (for `prepared.json`).
    pub stage_dir: PathBuf,
    /// Origin URL for creating `base` when it doesn't exist yet
    /// (auto-migration of pre-Phase-C installs). Derived from the live
    /// checkout's `origin` when `None`.
    pub origin_url: Option<String>,
    /// `julia` binary to instantiate environments with; `None` skips the
    /// Julia steps (frontend-only hosts).
    pub julia_bin: Option<String>,
    /// Run `npm ci` for the MathJax sidecar (best-effort, recorded).
    pub npm: bool,
}

/// Recorded outcome of a completed prepare — the arm/apply phases trust this
/// (plus a live HEAD re-check) instead of re-deriving state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedState {
    pub schema: u32,
    #[serde(flatten)]
    pub identity: ReleaseIdentity,
    /// Absolute path of the prepared worktree.
    pub checkout: PathBuf,
    /// The tag's commit — verified equal to the worktree HEAD.
    pub commit: String,
    /// Whether Julia envs were instantiated + load-tested in this worktree.
    pub julia_instantiated: bool,
    /// Whether the MathJax sidecar deps were installed.
    pub mathjax_deps: bool,
    /// Unix seconds when preparation completed.
    pub prepared_at: u64,
}

impl PreparedState {
    pub async fn write(&self, stage_dir: &Path) -> Result<()> {
        let tmp = stage_dir.join(format!("{PREPARED_MANIFEST}.tmp"));
        let text = serde_json::to_string_pretty(self).context("serializing prepared state")?;
        tokio::fs::write(&tmp, text).await.context("writing prepared state")?;
        tokio::fs::rename(&tmp, stage_dir.join(PREPARED_MANIFEST))
            .await
            .context("renaming prepared state into place")?;
        Ok(())
    }

    pub async fn read(stage_dir: &Path) -> Result<Self> {
        let path = stage_dir.join(PREPARED_MANIFEST);
        let text = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let s: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        s.identity.validate()?;
        Ok(s)
    }

    /// True when `stage_dir` records a completed prepare of exactly
    /// `identity` AND the worktree still exists at the recorded commit.
    pub async fn matches(stage_dir: &Path, identity: &ReleaseIdentity) -> bool {
        match Self::read(stage_dir).await {
            Ok(s) if s.identity == *identity => {
                head_commit(&s.checkout).await.as_deref() == Some(s.commit.as_str())
            }
            _ => false,
        }
    }
}

/// Prepare one version: ensure the base clone, fetch the tag, add a detached
/// worktree at its commit, optionally instantiate + load-test the Julia envs
/// and the MathJax sidecar deps, then record `prepared.json`. Idempotent —
/// a matching completed prepare short-circuits. Serialized across processes
/// via the staging lock (on an all-in-one install the FE and BE can both
/// reach prepare; the second entrant finds the recorded state and returns).
pub async fn prepare(spec: &PrepareSpec) -> Result<PreparedState> {
    spec.identity.validate()?;
    let updates_root = spec
        .stage_dir
        .parent()
        .ok_or_else(|| anyhow!("stage dir has no parent"))?
        .to_path_buf();
    let lock =
        crate::lock::StageLock::acquire(&updates_root, Duration::from_secs(600)).await?;
    let result = prepare_locked(spec).await;
    lock.release();
    result
}

async fn prepare_locked(spec: &PrepareSpec) -> Result<PreparedState> {
    if let Ok(existing) = PreparedState::read(&spec.stage_dir).await {
        if existing.identity == spec.identity
            && head_commit(&existing.checkout).await.as_deref() == Some(existing.commit.as_str())
            && (spec.julia_bin.is_none() || existing.julia_instantiated)
        {
            return Ok(existing);
        }
    }

    let base = spec.repo_dir.join("base");
    ensure_base(spec, &base).await?;

    git(&base, &["fetch", "--tags", "--force", "origin"], GIT_TIMEOUT)
        .await
        .context("fetching tags into base clone")?;

    let commit = rev_parse(&base, &format!("refs/tags/{}^{{commit}}", spec.identity.tag))
        .await
        .with_context(|| format!("tag {} not present after fetch", spec.identity.tag))?;

    let versions = spec.repo_dir.join("versions");
    tokio::fs::create_dir_all(&versions).await.context("creating versions dir")?;
    let checkout = versions.join(&spec.identity.tag);

    if checkout.exists() {
        match head_commit(&checkout).await {
            Some(head) if head == commit => {}
            _ => {
                // Partial or moved — rebuild it.
                tracing::warn!(dir = %checkout.display(), "removing incomplete version worktree");
                remove_worktree(&base, &checkout).await?;
            }
        }
    }
    if !checkout.exists() {
        // An externally deleted worktree stays registered; prune first so the
        // add can't die on "missing but already registered".
        let _ = git(&base, &["worktree", "prune"], GIT_TIMEOUT).await;
        git(
            &base,
            &[
                "worktree",
                "add",
                "--detach",
                &checkout.to_string_lossy(),
                &commit,
            ],
            GIT_TIMEOUT,
        )
        .await
        .context("adding version worktree")?;
    }
    // HEAD must equal the tag's commit (a moved tag or half-checkout dies
    // here, not at first use — same gate as install.sh).
    let head = head_commit(&checkout)
        .await
        .ok_or_else(|| anyhow!("prepared worktree has no HEAD"))?;
    if head != commit {
        bail!(
            "prepared worktree HEAD ({head}) != tag commit ({commit}) — refusing"
        );
    }

    let mut julia_instantiated = false;
    if let Some(julia) = &spec.julia_bin {
        for env in ["julia/kernel", "julia/repl", "julia/pluto"] {
            let project = checkout.join(env);
            if !project.exists() {
                continue;
            }
            run(
                julia,
                &[
                    &format!("--project={}", project.display()),
                    "-e",
                    "using Pkg; Pkg.instantiate()",
                ],
                JULIA_TIMEOUT,
                &format!("julia instantiate {env}"),
            )
            .await
            .with_context(|| format!("instantiating {env}"))?;
        }
        // Load-test: the envs must not just resolve but LOAD at this ref
        // (the release-blocking julia-check job's local equivalent).
        for (env, module) in [("julia/kernel", "ShipToolsKernel"), ("julia/repl", "ShipToolsRepl")] {
            let project = checkout.join(env);
            if !project.exists() {
                continue;
            }
            run(
                julia,
                &[
                    &format!("--project={}", project.display()),
                    "-e",
                    &format!("using {module}"),
                ],
                JULIA_TIMEOUT,
                &format!("julia load-test {module}"),
            )
            .await?;
        }
        julia_instantiated = true;
    }

    let mut mathjax_deps = false;
    if spec.npm {
        let sidecar = checkout.join("rust/backend/sidecars/mathjax");
        if sidecar.exists() {
            match run_in(&sidecar, "npm", &["ci", "--silent"], NPM_TIMEOUT, "npm ci (mathjax)").await
            {
                Ok(()) => mathjax_deps = true,
                // Best-effort, like install.sh: a box without node still
                // updates fine; math previews degrade until deps land.
                Err(e) => tracing::warn!(error = %e, "mathjax npm ci failed — math rendering unavailable in prepared version"),
            }
        }
    }

    let state = PreparedState {
        schema: 1,
        identity: spec.identity.clone(),
        checkout: checkout.clone(),
        commit,
        julia_instantiated,
        mathjax_deps,
        prepared_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    state.write(&spec.stage_dir).await?;
    tracing::info!(tag = %spec.identity.tag, checkout = %checkout.display(), julia = julia_instantiated, "version prepared");
    Ok(state)
}

/// Ensure the base clone exists. Created on demand (auto-migration of
/// pre-Phase-C installs): a blobless clone from the origin URL, falling back
/// to a full clone when the server rejects filters.
async fn ensure_base(spec: &PrepareSpec, base: &Path) -> Result<()> {
    if base.join(".git").exists() || base.join("HEAD").exists() {
        return Ok(());
    }
    let origin = match &spec.origin_url {
        Some(u) => u.clone(),
        None => {
            let current = spec.repo_dir.join("current");
            rev_origin(&current)
                .await
                .context("deriving origin URL from the live checkout (pass origin_url explicitly?)")?
        }
    };
    if let Some(parent) = base.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tracing::info!(origin = %origin, base = %base.display(), "creating base clone for versioned updates");
    let blobless = git_anywhere(
        &[
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            &origin,
            &base.to_string_lossy(),
        ],
        GIT_TIMEOUT,
    )
    .await;
    if blobless.is_err() {
        tracing::warn!("blobless clone failed; retrying as a full clone");
        let _ = tokio::fs::remove_dir_all(base).await;
        git_anywhere(
            &["clone", "--no-checkout", &origin, &base.to_string_lossy()],
            GIT_TIMEOUT,
        )
        .await
        .context("creating base clone")?;
    }
    Ok(())
}

/// Remove a version worktree properly (worktree remove + prune), falling back
/// to a plain delete + prune for a dir git no longer recognizes.
pub async fn remove_worktree(base: &Path, checkout: &Path) -> Result<()> {
    let res = git(
        base,
        &[
            "worktree",
            "remove",
            "--force",
            &checkout.to_string_lossy(),
        ],
        GIT_TIMEOUT,
    )
    .await;
    if res.is_err() && checkout.exists() {
        tokio::fs::remove_dir_all(checkout)
            .await
            .with_context(|| format!("removing {}", checkout.display()))?;
    }
    let _ = git(base, &["worktree", "prune"], GIT_TIMEOUT).await;
    Ok(())
}

async fn head_commit(dir: &Path) -> Option<String> {
    rev_parse(dir, "HEAD").await.ok()
}

async fn rev_parse(dir: &Path, what: &str) -> Result<String> {
    let out = git_capture(dir, &["rev-parse", what], GIT_TIMEOUT).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

async fn rev_origin(dir: &Path) -> Result<String> {
    let out = git_capture(dir, &["remote", "get-url", "origin"], GIT_TIMEOUT).await?;
    let url = String::from_utf8_lossy(&out).trim().to_string();
    if url.is_empty() {
        bail!("checkout at {} has no origin URL", dir.display());
    }
    Ok(url)
}

async fn git(dir: &Path, args: &[&str], timeout: Duration) -> Result<()> {
    git_capture(dir, args, timeout).await.map(|_| ())
}

async fn git_capture(dir: &Path, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    exec(cmd, timeout, &format!("git {}", args.first().unwrap_or(&""))).await
}

async fn git_anywhere(args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    exec(cmd, timeout, &format!("git {}", args.first().unwrap_or(&""))).await
}

/// Run a binary to completion (no working-dir change).
async fn run(bin: &str, args: &[&str], timeout: Duration, what: &str) -> Result<()> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    exec(cmd, timeout, what).await.map(|_| ())
}

/// Run a binary to completion in `dir`.
async fn run_in(dir: &Path, bin: &str, args: &[&str], timeout: Duration, what: &str) -> Result<()> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.current_dir(dir).args(args);
    exec(cmd, timeout, what).await.map(|_| ())
}

async fn exec(mut cmd: tokio::process::Command, timeout: Duration, what: &str) -> Result<Vec<u8>> {
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => bail!("{what} timed out after {}s", timeout.as_secs()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => bail!("{what}: binary not found"),
        Ok(Err(e)) => return Err(e).with_context(|| format!("spawning {what}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(3).collect();
        bail!(
            "{what} exit {:?}: {}",
            out.status.code(),
            tail.into_iter().rev().collect::<Vec<_>>().join(" | ")
        );
    }
    Ok(out.stdout)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::identity::ReleaseIdentity;

    fn sh(dir: &Path, script: &str) {
        let st = std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(st.success(), "script failed: {script}");
    }

    #[tokio::test]
    async fn prepare_creates_versioned_worktree_from_origin() {
        let base_tmp =
            std::env::temp_dir().join(format!("sot-updater-prep-{}", std::process::id()));
        let _ = tokio::fs::remove_dir_all(&base_tmp).await;
        tokio::fs::create_dir_all(&base_tmp).await.unwrap();

        // Fixture "origin" repo with a tagged release.
        let origin = base_tmp.join("origin");
        tokio::fs::create_dir_all(&origin).await.unwrap();
        sh(
            &origin,
            "git init -q -b main . && echo hello > README.md && git add . && git commit -qm init && git tag v9.9.9",
        );

        let prefix = base_tmp.join("prefix");
        let repo_dir = prefix.join("repo");
        let stage = prefix.join("updates").join("v9.9.9");
        tokio::fs::create_dir_all(&stage).await.unwrap();

        let spec = PrepareSpec {
            identity: ReleaseIdentity {
                repo: "kalidke/ship-of-tools".into(),
                tag: "v9.9.9".into(),
                version: "9.9.9".into(),
                target: "linux-x86_64".into(),
                asset: "sot-9.9.9-linux-x86_64.tar.gz".into(),
                asset_sha256: "ab".repeat(32),
            },
            repo_dir: repo_dir.clone(),
            stage_dir: stage.clone(),
            origin_url: Some(origin.to_string_lossy().into_owned()),
            julia_bin: None,
            npm: false,
        };

        let state = prepare(&spec).await.unwrap();
        assert_eq!(state.checkout, repo_dir.join("versions").join("v9.9.9"));
        assert!(state.checkout.join("README.md").exists());
        assert!(!state.julia_instantiated);
        // HEAD equals the tag's commit.
        let want = git_capture(&origin, &["rev-parse", "v9.9.9^{commit}"], GIT_TIMEOUT)
            .await
            .unwrap();
        assert_eq!(state.commit, String::from_utf8_lossy(&want).trim());

        // Idempotent: second call short-circuits on the recorded state.
        let again = prepare(&spec).await.unwrap();
        assert_eq!(again.commit, state.commit);
        assert!(PreparedState::matches(&stage, &spec.identity).await);

        // A vanished worktree is detected and rebuilt.
        tokio::fs::remove_dir_all(&state.checkout).await.unwrap();
        assert!(!PreparedState::matches(&stage, &spec.identity).await);
        let rebuilt = prepare(&spec).await.unwrap();
        assert_eq!(rebuilt.commit, state.commit);
        assert!(rebuilt.checkout.join("README.md").exists());

        tokio::fs::remove_dir_all(&base_tmp).await.unwrap();
    }
}
