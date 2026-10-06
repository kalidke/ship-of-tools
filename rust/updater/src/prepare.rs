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

use crate::Spawner;

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
        tokio::fs::write(&tmp, text)
            .await
            .context("writing prepared state")?;
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
        let s: Self =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        s.identity.validate()?;
        Ok(s)
    }

    /// True when `stage_dir` records a completed prepare of exactly
    /// `identity` AND the worktree still exists at the recorded commit with a
    /// clean tree (modified tracked files leave HEAD unchanged — cleanliness
    /// is part of "still what we prepared").
    pub async fn matches(
        spawner: &dyn Spawner,
        stage_dir: &Path,
        identity: &ReleaseIdentity,
    ) -> bool {
        match Self::read(stage_dir).await {
            Ok(s) if s.identity == *identity => {
                head_commit(spawner, &s.checkout).await.as_deref() == Some(s.commit.as_str())
                    && worktree_clean(spawner, &s.checkout).await
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
pub async fn prepare(spawner: &dyn Spawner, spec: &PrepareSpec) -> Result<PreparedState> {
    spec.identity.validate()?;
    let updates_root = spec
        .stage_dir
        .parent()
        .ok_or_else(|| anyhow!("stage dir has no parent"))?
        .to_path_buf();
    let lock = crate::lock::StageLock::acquire(&updates_root, Duration::from_secs(600)).await?;
    let result = prepare_locked(spawner, spec).await;
    lock.release();
    result
}

#[allow(
    clippy::too_many_lines,
    reason = "prepares the staged update under the lock, one stage per step; predates the 100-line limit"
)]
async fn prepare_locked(spawner: &dyn Spawner, spec: &PrepareSpec) -> Result<PreparedState> {
    if let Ok(existing) = PreparedState::read(&spec.stage_dir).await {
        if existing.identity == spec.identity
            && head_commit(spawner, &existing.checkout).await.as_deref()
                == Some(existing.commit.as_str())
            && worktree_clean(spawner, &existing.checkout).await
            && (spec.julia_bin.is_none() || existing.julia_instantiated)
        {
            return Ok(existing);
        }
    }

    let base = spec.repo_dir.join("base");
    ensure_base(spawner, spec, &base).await?;

    git(
        spawner,
        &base,
        &["fetch", "--tags", "--force", "origin"],
        GIT_TIMEOUT,
    )
    .await
    .context("fetching tags into base clone")?;

    let commit = rev_parse(
        spawner,
        &base,
        &format!("refs/tags/{}^{{commit}}", spec.identity.tag),
    )
    .await
    .with_context(|| format!("tag {} not present after fetch", spec.identity.tag))?;

    // Commit binding (moved-tag defense): when the staged release published a
    // COMMIT file, the tag we are about to check out MUST resolve to exactly
    // that commit — otherwise the verified binaries and the checkout come
    // from different sources and we refuse to prepare (and thus to arm).
    if let Ok(ready) = crate::manifest::ReadyManifest::read(&spec.stage_dir).await {
        if let Some(src) = &ready.source_commit {
            if *src != commit {
                bail!(
                    "tag {} resolves to {commit} but the release was built from {src} — moved tag, refusing",
                    spec.identity.tag
                );
            }
        }
    }

    let versions = spec.repo_dir.join("versions");
    tokio::fs::create_dir_all(&versions)
        .await
        .context("creating versions dir")?;
    let checkout = versions.join(&spec.identity.tag);

    if checkout.exists() {
        match head_commit(spawner, &checkout).await {
            Some(head) if head == commit => {}
            _ => {
                // Partial or moved — rebuild it.
                tracing::warn!(dir = %checkout.display(), "removing incomplete version worktree");
                remove_worktree(spawner, &base, &checkout).await?;
            }
        }
    }
    if !checkout.exists() {
        // An externally deleted worktree stays registered; prune first so the
        // add can't die on "missing but already registered".
        let _ = git(spawner, &base, &["worktree", "prune"], GIT_TIMEOUT).await;
        git(
            spawner,
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
    let head = head_commit(spawner, &checkout)
        .await
        .ok_or_else(|| anyhow!("prepared worktree has no HEAD"))?;
    if head != commit {
        bail!("prepared worktree HEAD ({head}) != tag commit ({commit}) — refusing");
    }

    let mut julia_instantiated = false;
    if let Some(julia) = &spec.julia_bin {
        for env in ["julia/kernel", "julia/repl", "julia/pluto"] {
            let project = checkout.join(env);
            if !project.exists() {
                continue;
            }
            run(
                spawner,
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
        // BETWEEN instantiate and the load test: Pkg can rewrite a tracked
        // Project.toml while resolving, and the load test must prove the
        // tree as it ships, not the tree Pkg just left behind.
        restore_to_tag(spawner, &checkout, &spec.identity.tag).await?;
        // Load-test: the envs must not just resolve but LOAD at this ref
        // (the release-blocking julia-check job's local equivalent).
        for (env, module) in [
            ("julia/kernel", "ShipToolsKernel"),
            ("julia/repl", "ShipToolsRepl"),
        ] {
            let project = checkout.join(env);
            if !project.exists() {
                continue;
            }
            run(
                spawner,
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
            match run_in(
                spawner,
                &sidecar,
                "npm",
                &["ci", "--silent"],
                NPM_TIMEOUT,
                "npm ci (mathjax)",
            )
            .await
            {
                Ok(()) => mathjax_deps = true,
                // Best-effort, like install.sh: a box without node still
                // updates fine; math previews degrade until deps land.
                Err(e) => {
                    tracing::warn!(error = %e, "mathjax npm ci failed — math rendering unavailable in prepared version")
                }
            }
        }
    }

    // Second call, same idempotent function: the load test or the mathjax
    // `npm ci` step could in principle leave a tracked file dirty too, and an
    // unappliable pointer must never be armed. On the normal clean tree this
    // is one `git status` and nothing else.
    restore_to_tag(spawner, &checkout, &spec.identity.tag).await?;

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
async fn ensure_base(spawner: &dyn Spawner, spec: &PrepareSpec, base: &Path) -> Result<()> {
    if base.join(".git").exists() || base.join("HEAD").exists() {
        return Ok(());
    }
    let origin = match &spec.origin_url {
        Some(u) => u.clone(),
        None => {
            let current = spec.repo_dir.join("current");
            rev_origin(spawner, &current).await.context(
                "deriving origin URL from the live checkout (pass origin_url explicitly?)",
            )?
        }
    };
    if let Some(parent) = base.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }
    tracing::info!(origin = %origin, base = %base.display(), "creating base clone for versioned updates");
    let blobless = git_anywhere(
        spawner,
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
            spawner,
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
pub(crate) async fn remove_worktree(
    spawner: &dyn Spawner,
    base: &Path,
    checkout: &Path,
) -> Result<()> {
    let res = git(
        spawner,
        base,
        &["worktree", "remove", "--force", &checkout.to_string_lossy()],
        GIT_TIMEOUT,
    )
    .await;
    if res.is_err() && checkout.exists() {
        tokio::fs::remove_dir_all(checkout)
            .await
            .with_context(|| format!("removing {}", checkout.display()))?;
    }
    let _ = git(spawner, base, &["worktree", "prune"], GIT_TIMEOUT).await;
    Ok(())
}

async fn head_commit(spawner: &dyn Spawner, dir: &Path) -> Option<String> {
    rev_parse(spawner, dir, "HEAD").await.ok()
}

/// No modified TRACKED files (`-uno`: untracked build products — julia
/// Manifests, mathjax node_modules — are expected in a prepared worktree).
async fn worktree_clean(spawner: &dyn Spawner, dir: &Path) -> bool {
    match git_capture(
        spawner,
        dir,
        &["status", "--porcelain", "-uno"],
        GIT_TIMEOUT,
    )
    .await
    {
        Ok(out) => out.iter().all(|b| b.is_ascii_whitespace()),
        Err(_) => false,
    }
}

/// Put the prepared worktree back at its tag: Pkg can rewrite a tracked
/// Project.toml while resolving (see `.gitattributes:9-13`), and an
/// unappliable pointer must never be armed — fail the prepare loudly instead.
async fn restore_to_tag(spawner: &dyn Spawner, checkout: &Path, tag: &str) -> Result<()> {
    // Name what is about to be discarded. On a platform where instantiate
    // rewrites a tracked file, this line is the ONLY record of which file it
    // was — the restore below erases the evidence. Best-effort: a failed
    // status must not fail the prepare, the restore is what matters. The
    // normal case is a clean tree: one subprocess, nothing left to restore.
    if let Ok(out) = git_capture(
        spawner,
        checkout,
        &["status", "--porcelain", "-uno"],
        GIT_TIMEOUT,
    )
    .await
    {
        let listed = String::from_utf8_lossy(&out);
        let listed = listed.trim();
        if listed.is_empty() {
            return Ok(());
        }
        tracing::info!(
            tag = %tag,
            files = %listed.replace('\n', "; "),
            "prepared worktree: restoring tracked files modified during prepare"
        );
    }
    git(spawner, checkout, &["checkout", "--", "."], GIT_TIMEOUT)
        .await
        .with_context(|| format!("restoring tracked files in {}", checkout.display()))?;
    if !worktree_clean(spawner, checkout).await {
        bail!(
            "prepared worktree {} still has modified tracked files — it does not match tag {tag}, so it will not be armed",
            checkout.display()
        );
    }
    Ok(())
}

async fn rev_parse(spawner: &dyn Spawner, dir: &Path, what: &str) -> Result<String> {
    let out = git_capture(spawner, dir, &["rev-parse", what], GIT_TIMEOUT).await?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

async fn rev_origin(spawner: &dyn Spawner, dir: &Path) -> Result<String> {
    let out = git_capture(spawner, dir, &["remote", "get-url", "origin"], GIT_TIMEOUT).await?;
    let url = String::from_utf8_lossy(&out).trim().to_string();
    if url.is_empty() {
        bail!("checkout at {} has no origin URL", dir.display());
    }
    Ok(url)
}

async fn git(spawner: &dyn Spawner, dir: &Path, args: &[&str], timeout: Duration) -> Result<()> {
    git_capture(spawner, dir, args, timeout).await.map(|_| ())
}

async fn git_capture(
    spawner: &dyn Spawner,
    dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    exec(
        spawner,
        cmd,
        timeout,
        &format!("git {}", args.first().unwrap_or(&"")),
    )
    .await
}

async fn git_anywhere(spawner: &dyn Spawner, args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    exec(
        spawner,
        cmd,
        timeout,
        &format!("git {}", args.first().unwrap_or(&"")),
    )
    .await
}

/// Run a binary to completion (no working-dir change).
async fn run(
    spawner: &dyn Spawner,
    bin: &str,
    args: &[&str],
    timeout: Duration,
    what: &str,
) -> Result<()> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args);
    exec(spawner, cmd, timeout, what).await.map(|_| ())
}

/// Run a binary to completion in `dir`.
async fn run_in(
    spawner: &dyn Spawner,
    dir: &Path,
    bin: &str,
    args: &[&str],
    timeout: Duration,
    what: &str,
) -> Result<()> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.current_dir(dir).args(args);
    exec(spawner, cmd, timeout, what).await.map(|_| ())
}

async fn exec(
    spawner: &dyn Spawner,
    mut cmd: tokio::process::Command,
    timeout: Duration,
    what: &str,
) -> Result<Vec<u8>> {
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let out = match tokio::time::timeout(timeout, spawner.output(&mut cmd)).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::tests::{output, RecordingSpawner};
    use std::sync::Mutex;

    const KERNEL_PROJECT: &str = "name = \"ShipToolsKernel\"\n";
    const COMMIT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Fixture {
        root: PathBuf,
        spec: PrepareSpec,
        mode: &'static str,
        dirty: Mutex<bool>,
        commands: Mutex<Vec<Vec<String>>>,
    }

    impl Fixture {
        fn new(mode: &'static str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("sot-updater-prepare-{}", crate::unique::suffix()));
            let spec = PrepareSpec {
                identity: ReleaseIdentity {
                    repo: "example/project".into(),
                    tag: "v9.9.9".into(),
                    version: "9.9.9".into(),
                    target: "linux-x86_64".into(),
                    asset: "sot-9.9.9-linux-x86_64.tar.gz".into(),
                    asset_sha256: "ab".repeat(32),
                },
                repo_dir: root.join("repo"),
                stage_dir: root.join("updates/v9.9.9"),
                origin_url: Some("https://example.invalid/project".into()),
                julia_bin: (mode != "none").then(|| "julia".into()),
                npm: true,
            };
            std::fs::create_dir_all(&spec.stage_dir).unwrap();
            Self {
                root,
                spec,
                mode,
                dirty: Mutex::new(false),
                commands: Mutex::new(Vec::new()),
            }
        }

        fn respond(
            &self,
            command: &tokio::process::Command,
        ) -> std::io::Result<std::process::Output> {
            let cmd = command.as_std();
            let bin = cmd.get_program().to_str().unwrap();
            let args: Vec<String> = cmd
                .get_args()
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            self.commands.lock().unwrap().push(
                std::iter::once(bin.to_string())
                    .chain(args.clone())
                    .collect(),
            );
            match bin {
                "git" => self.git(&args),
                "julia" => {
                    let project =
                        PathBuf::from(args[0].strip_prefix("--project=").expect("Julia project"));
                    assert_eq!(args[1], "-e");
                    let instantiate = args[2].contains("Pkg.instantiate");
                    if (self.mode == "instantiate" && instantiate)
                        || (self.mode == "load" && !instantiate)
                    {
                        std::fs::write(project.join("Project.toml"), "# rewritten\n")?;
                        *self.dirty.lock().unwrap() = true;
                    }
                    if self.mode == "untracked" {
                        std::fs::write(project.join("Manifest.toml"), "build product")?;
                    }
                    if !instantiate {
                        std::fs::copy(project.join("Project.toml"), self.root.join("witness"))?;
                    }
                    Ok(output(0, Vec::new()))
                }
                "npm" => {
                    assert_eq!(args, ["ci", "--silent"]);
                    Ok(output(0, Vec::new()))
                }
                _ => panic!("unexpected preparation command {bin}: {args:?}"),
            }
        }

        fn git(&self, args: &[String]) -> std::io::Result<std::process::Output> {
            let (dir, args) = if args[0] == "-C" {
                (Path::new(&args[1]), &args[2..])
            } else {
                (self.root.as_path(), args)
            };
            match args[0].as_str() {
                "clone" => {
                    std::fs::create_dir_all(Path::new(args.last().unwrap()).join(".git"))?;
                }
                "fetch" => {
                    assert_eq!(args, ["fetch", "--tags", "--force", "origin"]);
                }
                "rev-parse" => {
                    return Ok(output(
                        if dir.exists() { 0 } else { 1 },
                        if dir.exists() { COMMIT.as_bytes() } else { b"" },
                    ));
                }
                "status" => {
                    assert_eq!(args, ["status", "--porcelain", "-uno"]);
                    return Ok(output(
                        0,
                        if *self.dirty.lock().unwrap() {
                            b" M Project.toml\n".as_slice()
                        } else {
                            b""
                        },
                    ));
                }
                "checkout" => {
                    assert_eq!(args, ["checkout", "--", "."]);
                    if self.mode != "irreparable" {
                        for env in ["julia/kernel", "julia/repl", "julia/pluto"] {
                            std::fs::write(dir.join(env).join("Project.toml"), KERNEL_PROJECT)?;
                        }
                        *self.dirty.lock().unwrap() = false;
                    }
                }
                "worktree" => match args[1].as_str() {
                    "add" => {
                        assert_eq!(args[2], "--detach");
                        assert_eq!(args[4], COMMIT);
                        let checkout = Path::new(&args[3]);
                        for env in ["julia/kernel", "julia/repl", "julia/pluto"] {
                            std::fs::create_dir_all(checkout.join(env))?;
                            std::fs::write(
                                checkout.join(env).join("Project.toml"),
                                KERNEL_PROJECT,
                            )?;
                        }
                        std::fs::create_dir_all(checkout.join("rust/backend/sidecars/mathjax"))?;
                        std::fs::write(checkout.join("README.md"), "hello")?;
                    }
                    "prune" => assert_eq!(args.len(), 2),
                    "remove" => {
                        assert_eq!(args[2], "--force");
                        std::fs::remove_dir_all(&args[3])?;
                    }
                    _ => panic!("unexpected worktree request: {args:?}"),
                },
                _ => panic!("unexpected git request: {args:?}"),
            }
            Ok(output(0, Vec::new()))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).expect("fixture cleanup");
        }
    }

    #[tokio::test]
    async fn prepare_creates_versioned_worktree_from_origin() {
        let fixture = Fixture::new("none");
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
        let state = prepare(&spawner, &fixture.spec).await.unwrap();
        assert_eq!(
            state.checkout,
            fixture.spec.repo_dir.join("versions/v9.9.9")
        );
        assert!(state.checkout.join("README.md").exists());
        assert!(!state.julia_instantiated);
        assert_eq!(state.commit, COMMIT);
        assert!(state.mathjax_deps);
        let again = prepare(&spawner, &fixture.spec).await.unwrap();
        assert_eq!(again.commit, state.commit);
        assert!(
            PreparedState::matches(&spawner, &fixture.spec.stage_dir, &fixture.spec.identity).await
        );
        std::fs::remove_dir_all(&state.checkout).unwrap();
        assert!(
            !PreparedState::matches(&spawner, &fixture.spec.stage_dir, &fixture.spec.identity)
                .await
        );
        let rebuilt = prepare(&spawner, &fixture.spec).await.unwrap();
        assert_eq!(rebuilt.commit, state.commit);
        assert!(rebuilt.checkout.join("README.md").exists());
        let requests = fixture.commands.lock().unwrap();
        for action in ["clone", "fetch", "worktree", "rev-parse", "status"] {
            assert!(
                requests.iter().any(|r| r.iter().any(|a| a == action)),
                "missing {action} request"
            );
        }
    }

    #[tokio::test]
    async fn prepare_restores_a_tracked_file_the_env_step_rewrote() {
        let fixture = Fixture::new("instantiate");
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
        let state = prepare(&spawner, &fixture.spec).await.unwrap();
        assert!(state.julia_instantiated);
        assert_eq!(
            std::fs::read_to_string(state.checkout.join("julia/kernel/Project.toml")).unwrap(),
            KERNEL_PROJECT
        );
        assert_eq!(std::fs::read_to_string(fixture.root.join("witness")).unwrap(), KERNEL_PROJECT,
            "the load test ran against a still-rewritten Project.toml — restore must happen before the load test, not after");
    }

    #[tokio::test]
    async fn prepare_restores_a_tracked_file_the_load_test_rewrote() {
        let fixture = Fixture::new("load");
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
        let state = prepare(&spawner, &fixture.spec).await.unwrap();
        assert!(state.julia_instantiated);
        assert!(
            worktree_clean(&spawner, &state.checkout).await,
            "a file the load test dirtied must still be restored before the version is armed"
        );
    }

    #[tokio::test]
    async fn prepare_keeps_untracked_build_products() {
        let fixture = Fixture::new("untracked");
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
        let state = prepare(&spawner, &fixture.spec).await.unwrap();
        assert!(
            state.checkout.join("julia/kernel/Manifest.toml").exists(),
            "restoring tracked files must not sweep what instantiate produced"
        );
        assert!(worktree_clean(&spawner, &state.checkout).await);
    }

    #[tokio::test]
    async fn restore_refuses_dirt_it_cannot_repair() {
        let fixture = Fixture::new("irreparable");
        let spawner = RecordingSpawner(|cmd: &tokio::process::Command| fixture.respond(cmd));
        *fixture.dirty.lock().unwrap() = true;
        let err = restore_to_tag(&spawner, &fixture.root, "v9.9.9")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("v9.9.9"), "must name the tag: {err}");
        assert!(err.contains("will not be armed"), "{err}");
    }
}
