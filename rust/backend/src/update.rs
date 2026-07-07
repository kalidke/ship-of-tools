// update.rs — ADR 0030 §4 Phase-C updater CORE: check + notify + stage.
//
// Scope of THIS module: discover a newer release, tell the frontend(s) about
// it, and stage the platform binary (download → sha256-verify → unpack) into a
// pending dir. It does NOT apply or restart — that is the ADR 0017 supervisor /
// systemd `sotd.service` path, which picks up the staged bits on the next
// launch. Keeping "put new bits where the launcher already looks" separate from
// "check + download" is the whole design of ADR 0030 §4.
//
// HARD GUARD: a build whose `app_version()` carries the `-dev` marker never
// checks and never stages — the updater must not clobber a locally built
// binary. This is unconditional and independent of config.
//
// Because the release repo is private, both the check and the download shell
// out to the authenticated `gh` CLI (present on the dev/canary boxes). Native
// HTTP is the public-phase story; the `Fetcher` enum is the seam where that
// second backend lands without touching the rest of this module.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use sot_protocol::{app_version, op, FeCommandEvt, Frame, UpdateCheckRes};
use tokio::sync::broadcast;

use crate::handlers::HandlerOutput;

/// Default release repo. Overridable via `SOT_UPDATE_REPO` for testing against
/// a fork.
const DEFAULT_REPO: &str = "kalidke/ship-of-tools";

/// Delay before the first automatic check after boot, then the steady cadence.
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);

/// Per-daemon lock serializing staging so the periodic task and an on-demand
/// `update.check` can never download/unpack the same release concurrently.
static STAGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Marker file written into a staging dir once its asset is verified + unpacked.
/// Its presence is the idempotency signal (`is_staged`).
const STAGED_MARKER: &str = ".sot-staged";

// ─── Config ─────────────────────────────────────────────────────────────

/// Update behavior from `SOT_UPDATE_MODE`. `auto` is reserved (ADR 0030 §4);
/// until apply-at-stage lands it is treated as `notify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Notify,
}

fn mode_from_env() -> Mode {
    match std::env::var("SOT_UPDATE_MODE").ok().as_deref().map(str::trim) {
        Some("off") => Mode::Off,
        // "auto" is reserved; treat it as notify for now (no stage-time apply).
        Some("auto") | Some("notify") | None | Some("") => Mode::Notify,
        Some(other) => {
            tracing::warn!(value = %other, "unknown SOT_UPDATE_MODE; defaulting to notify");
            Mode::Notify
        }
    }
}

fn repo_from_env() -> String {
    std::env::var("SOT_UPDATE_REPO")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

// ─── Platform asset naming ──────────────────────────────────────────────

#[cfg(target_os = "linux")]
const TARGET_OS: &str = "linux";
#[cfg(target_os = "macos")]
const TARGET_OS: &str = "macos";
#[cfg(target_os = "windows")]
const TARGET_OS: &str = "windows";
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const TARGET_OS: &str = "unknown";

#[cfg(target_arch = "x86_64")]
const TARGET_ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const TARGET_ARCH: &str = "aarch64";
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const TARGET_ARCH: &str = "unknown";

/// Release-asset filename for a given version + target triple, or `None` for a
/// platform the release matrix doesn't ship (ADR 0030 §3: linux-x86_64 +
/// windows-x86_64 blocking, macos-aarch64 experimental). `version` is the bare
/// `X.Y.Z` (no leading `v`). Examples:
///   ("0.2.0","linux","x86_64")   → "sot-0.2.0-linux-x86_64.tar.gz"
///   ("0.2.0","windows","x86_64") → "sot-0.2.0-windows-x86_64.zip"
///   ("0.2.0","macos","aarch64")  → "sot-0.2.0-macos-aarch64.tar.gz"
fn asset_name_for(version: &str, os: &str, arch: &str) -> Option<String> {
    let (plat, ext) = match (os, arch) {
        ("linux", "x86_64") => ("linux-x86_64", "tar.gz"),
        ("windows", "x86_64") => ("windows-x86_64", "zip"),
        ("macos", "aarch64") => ("macos-aarch64", "tar.gz"),
        _ => return None,
    };
    Some(format!("sot-{version}-{plat}.{ext}"))
}

/// Asset filename for the running platform, or `None` if this build's triple
/// isn't in the release matrix.
fn platform_asset(version: &str) -> Option<String> {
    asset_name_for(version, TARGET_OS, TARGET_ARCH)
}

// ─── Staging paths ──────────────────────────────────────────────────────

/// `<data>/sot/updates` per ADR 0030's install layout: `~/.local/share/sot`
/// on Linux/macOS, `%LOCALAPPDATA%\sot` on Windows. Kept dependency-free (no
/// dirs crate) and consistent with `paths.rs`, which resolves via env vars.
fn updates_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(la) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(la).join("sot").join("updates");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(x).join("sot").join("updates");
        }
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h)
                .join(".local")
                .join("share")
                .join("sot")
                .join("updates");
        }
    }
    std::env::temp_dir().join("sot").join("updates")
}

fn stage_dir(tag: &str) -> PathBuf {
    updates_root().join(tag)
}

/// True once `tag` has been fully staged (asset verified + unpacked). Cheap
/// filesystem check; the marker is written last so a partial stage never reads
/// as done.
fn is_staged(tag: &str) -> bool {
    stage_dir(tag).join(STAGED_MARKER).exists()
}

// ─── Fetch backend seam ─────────────────────────────────────────────────

/// Swappable release-fetch backend. Only `Gh` is implemented — the release
/// repo is private, so check + download must ride the authenticated `gh` CLI.
/// The public phase adds an `Http` variant here (native HTTPS to the Releases
/// API + asset URLs) with no change to the calling code.
#[derive(Debug, Clone, Copy)]
enum Fetcher {
    Gh,
}

/// What a "latest release" query yields: the tag (`vX.Y.Z`) and the release's
/// asset filenames (so staging can confirm this platform's asset exists).
#[derive(Debug, Clone)]
struct ReleaseInfo {
    tag: String,
    assets: Vec<String>,
}

impl Fetcher {
    async fn latest(&self, repo: &str) -> Result<ReleaseInfo> {
        match self {
            Fetcher::Gh => gh_latest(repo).await,
        }
    }

    /// Download `patterns` (asset globs) for `tag` into `dest`.
    async fn download(&self, repo: &str, tag: &str, patterns: &[&str], dest: &Path) -> Result<()> {
        match self {
            Fetcher::Gh => gh_download(repo, tag, patterns, dest).await,
        }
    }
}

/// Run `gh` with a timeout, mapping a missing binary to a clear error. Returns
/// stdout bytes on success; a nonzero exit surfaces the first stderr line.
async fn run_gh(args: &[&str], timeout: Duration) -> Result<Vec<u8>> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args);
    cmd.stdin(std::process::Stdio::null());
    let fut = cmd.output();
    let out = match tokio::time::timeout(timeout, fut).await {
        Err(_) => bail!("timeout"),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => bail!("gh not found"),
        Ok(Err(e)) => return Err(e).context("spawning gh"),
        Ok(Ok(o)) => o,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        bail!("gh failed: {}", if first.is_empty() { "unknown error" } else { first });
    }
    Ok(out.stdout)
}

async fn gh_latest(repo: &str) -> Result<ReleaseInfo> {
    let path = format!("repos/{repo}/releases/latest");
    let stdout = run_gh(&["api", &path], Duration::from_secs(30)).await?;
    let v: serde_json::Value =
        serde_json::from_slice(&stdout).context("parsing gh api JSON")?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("release JSON has no tag_name"))?
        .to_string();
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok(ReleaseInfo { tag, assets })
}

async fn gh_download(repo: &str, tag: &str, patterns: &[&str], dest: &Path) -> Result<()> {
    let dest_s = dest.to_string_lossy().to_string();
    let mut args: Vec<String> = vec![
        "release".into(),
        "download".into(),
        tag.into(),
        "--repo".into(),
        repo.into(),
        "--dir".into(),
        dest_s,
        "--clobber".into(),
    ];
    for p in patterns {
        args.push("--pattern".into());
        args.push((*p).into());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // Downloads can be tens of MB; give them a generous ceiling.
    run_gh(&arg_refs, Duration::from_secs(300)).await?;
    Ok(())
}

// ─── Updater ────────────────────────────────────────────────────────────

/// The updater. Cheap to construct from env (config is process-static), so the
/// periodic task and each on-demand op make their own.
#[derive(Debug, Clone)]
pub struct Updater {
    /// Running product version (`app_version()`), e.g. `0.2.0` or
    /// `0.2.0-dev+abc`.
    current: String,
    /// True when `current` carries the `-dev` marker — the hard guard.
    dev: bool,
    mode: Mode,
    repo: String,
    fetch: Fetcher,
}

/// Outcome of a check, carrying both what the response needs and what staging
/// needs (the tag + asset list).
struct CheckOutcome {
    /// Latest version with the tag's leading `v` stripped; empty when the
    /// check couldn't run.
    latest: String,
    /// Full release tag (`vX.Y.Z`) — staging keys on this. Empty when no check.
    tag: String,
    assets: Vec<String>,
    update_available: bool,
    status: String,
}

impl Updater {
    pub fn from_env() -> Self {
        let current = app_version();
        let dev = current.contains("-dev");
        Self {
            dev,
            mode: mode_from_env(),
            repo: repo_from_env(),
            fetch: Fetcher::Gh,
            current,
        }
    }

    /// Query the latest release and compare against `current`. Never errors:
    /// a dev build / mode=off / unreachable gh all map to a structured status.
    async fn check(&self) -> CheckOutcome {
        if self.dev {
            return CheckOutcome {
                latest: String::new(),
                tag: String::new(),
                assets: Vec::new(),
                update_available: false,
                status: "disabled: dev build".into(),
            };
        }
        if self.mode == Mode::Off {
            return CheckOutcome {
                latest: String::new(),
                tag: String::new(),
                assets: Vec::new(),
                update_available: false,
                status: "disabled: update mode off".into(),
            };
        }
        match self.fetch.latest(&self.repo).await {
            Ok(info) => {
                let latest = strip_v(&info.tag).to_string();
                let available = compare_versions(&latest, &self.current) == Ordering::Greater;
                CheckOutcome {
                    latest,
                    tag: info.tag,
                    assets: info.assets,
                    update_available: available,
                    status: "ok".into(),
                }
            }
            Err(e) => CheckOutcome {
                latest: String::new(),
                tag: String::new(),
                assets: Vec::new(),
                update_available: false,
                status: format!("check unavailable: {e}"),
            },
        }
    }

    /// Download → verify → unpack the platform asset for `tag`. Idempotent
    /// (a completed stage short-circuits) and serialized process-wide via
    /// `STAGE_LOCK`. Returns `Ok(true)` when the stage is present afterward.
    async fn stage(&self, tag: &str, assets: &[String]) -> Result<bool> {
        let version = strip_v(tag);
        let asset = platform_asset(version)
            .ok_or_else(|| anyhow!("no release asset for this platform ({TARGET_OS}-{TARGET_ARCH})"))?;
        if !assets.iter().any(|a| a == &asset) {
            bail!("release {tag} has no asset {asset}");
        }
        if is_staged(tag) {
            return Ok(true);
        }
        let _lock = STAGE_LOCK.lock().await;
        // Re-check under the lock: a concurrent stage may have finished while
        // we waited.
        if is_staged(tag) {
            return Ok(true);
        }
        let dest = stage_dir(tag);
        tokio::fs::create_dir_all(&dest)
            .await
            .with_context(|| format!("creating staging dir {}", dest.display()))?;
        self.fetch
            .download(&self.repo, tag, &[asset.as_str(), "SHA256SUMS"], &dest)
            .await
            .context("downloading release assets")?;
        verify_sha256(&dest, &asset)
            .await
            .with_context(|| format!("verifying {asset}"))?;
        unpack(&dest.join(&asset), &dest)
            .await
            .with_context(|| format!("unpacking {asset}"))?;
        tokio::fs::write(dest.join(STAGED_MARKER), format!("{tag}\n"))
            .await
            .context("writing staged marker")?;
        tracing::info!(tag = %tag, asset = %asset, dir = %dest.display(), "update staged");
        Ok(true)
    }
}

/// Notify text ADR 0030 §4 specifies.
fn notify_text(latest: &str, current: &str) -> String {
    format!(
        "Ship of Tools v{} available (running v{}) — it will stage in the background",
        latest,
        strip_v(current)
    )
}

// ─── Periodic (daily) check task ────────────────────────────────────────

/// Spawn the background check task: first check ~2 min after boot, then every
/// 24 h. On a newer release it logs, broadcasts an `FE_COMMAND` `notify` to all
/// connected FEs (via the existing ADR 0025 channel — the identical mechanism
/// `fe.command.send` uses), then stages. Emits exactly one boot log describing
/// the updater's state (the ADR-required dev-build info line lives here).
pub fn spawn_periodic(fe_command_tx: broadcast::Sender<FeCommandEvt>) {
    let updater = Updater::from_env();
    if updater.dev {
        tracing::info!(
            version = %updater.current,
            "auto-update disabled: dev build (hard guard — never self-updates)"
        );
        return;
    }
    if updater.mode == Mode::Off {
        tracing::info!("auto-update disabled: SOT_UPDATE_MODE=off");
        return;
    }
    tracing::info!(
        repo = %updater.repo,
        current = %updater.current,
        "auto-update active (mode=notify); first check in ~2min, then daily"
    );
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            run_check_once(&updater, &fe_command_tx).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// One check cycle for the periodic task: check, and on a newer release notify
/// + stage. All failures degrade to a log line; the task never dies.
async fn run_check_once(updater: &Updater, fe_command_tx: &broadcast::Sender<FeCommandEvt>) {
    let out = updater.check().await;
    if out.update_available {
        tracing::info!(latest = %out.latest, current = %updater.current, "update available");
        let evt = FeCommandEvt {
            v: 1,
            cmd: "notify".into(),
            args: json!({ "text": notify_text(&out.latest, &updater.current) }),
            target: None,
        };
        // Fire-and-forget broadcast; a send error just means no FE is attached.
        let _ = fe_command_tx.send(evt);
        match updater.stage(&out.tag, &out.assets).await {
            Ok(_) => {}
            Err(e) => tracing::warn!(tag = %out.tag, error = %e, "staging update failed"),
        }
    } else if out.status.starts_with("check unavailable") {
        tracing::warn!(status = %out.status, "update check could not run");
    } else {
        tracing::debug!(latest = %out.latest, status = %out.status, "no update available");
    }
}

// ─── On-demand op handler ───────────────────────────────────────────────

/// `update.check` op (ADR 0030 §4). Runs the check synchronously (fast — one
/// `gh api` call), reports current/latest/availability + whether the release is
/// already staged, and — when an update is available but not yet staged — kicks
/// a background stage so the response isn't blocked on a multi-MB download.
/// Never errors on a failed check: the failure rides in `status`.
pub async fn handle_update_check(req_id: u64) -> Result<HandlerOutput> {
    let updater = Updater::from_env();
    let out = updater.check().await;
    let staged = !out.tag.is_empty() && is_staged(&out.tag);

    if out.update_available && !staged {
        // Fire-and-forget: make progress without holding the op response open.
        let updater2 = updater.clone();
        let tag = out.tag.clone();
        let assets = out.assets.clone();
        tokio::spawn(async move {
            if let Err(e) = updater2.stage(&tag, &assets).await {
                tracing::warn!(tag = %tag, error = %e, "on-demand staging failed");
            }
        });
    }

    let res = UpdateCheckRes {
        current: updater.current.clone(),
        latest: out.latest,
        update_available: out.update_available,
        staged,
        status: out.status,
    };
    Ok(vec![(
        Frame::res(req_id, op::UPDATE_CHECK, serde_json::to_value(res)?),
        None,
    )])
}

// ─── Semver ─────────────────────────────────────────────────────────────

/// Strip a single leading `v` from a tag (`v0.2.0` → `0.2.0`).
fn strip_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

#[derive(Debug, PartialEq, Eq)]
struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    /// Pre-release identifiers (empty = a release, which outranks any
    /// pre-release of the same core version).
    pre: Vec<String>,
}

/// Parse `X.Y.Z` with an optional `-prerelease` and optional `+build`
/// (build metadata is ignored). A leading `v` and a `-dev+<sha>` suffix both
/// parse — `0.2.0-dev+abc` → core 0.2.0, pre `["dev"]`. `None` on anything
/// that isn't three numeric core components.
fn parse_semver(s: &str) -> Option<SemVer> {
    let s = strip_v(s.trim());
    // Drop build metadata first so a `-dev+sha` doesn't fold the sha into pre.
    let s = s.split('+').next().unwrap_or(s);
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, p.split('.').map(str::to_string).collect()),
        None => (s, Vec::new()),
    };
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None; // more than three core components
    }
    Some(SemVer {
        major,
        minor,
        patch,
        pre,
    })
}

/// Compare two pre-release identifier lists per semver §11: a release (empty)
/// outranks any pre-release; otherwise compare identifiers left-to-right
/// (numeric < alphanumeric; numeric compared as ints; more identifiers wins a
/// common prefix).
fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater, // a is a release, b a pre-release
        (false, true) => Ordering::Less,
        (false, false) => {
            for (x, y) in a.iter().zip(b.iter()) {
                let o = cmp_ident(x, y);
                if o != Ordering::Equal {
                    return o;
                }
            }
            a.len().cmp(&b.len())
        }
    }
}

fn cmp_ident(x: &str, y: &str) -> Ordering {
    match (x.parse::<u64>(), y.parse::<u64>()) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Ok(_), Err(_)) => Ordering::Less, // numeric identifiers rank lower
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => x.cmp(y),
    }
}

/// Order two version strings. An unparsable side ranks below a parsable one
/// (and two unparsable are Equal) so a garbage "latest" can never be reported
/// as an available update.
fn compare_versions(a: &str, b: &str) -> Ordering {
    match (parse_semver(a), parse_semver(b)) {
        (Some(x), Some(y)) => x
            .major
            .cmp(&y.major)
            .then(x.minor.cmp(&y.minor))
            .then(x.patch.cmp(&y.patch))
            .then_with(|| cmp_pre(&x.pre, &y.pre)),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

// ─── sha256 (self-contained, no crypto dep) ─────────────────────────────

/// Verify that `<dir>/<asset>`'s sha256 matches its `SHA256SUMS` line. The sums
/// file format is `<hex>  <name>` (two spaces) or `<hex> *<name>` (binary
/// marker); we match on the basename. Errors if the asset isn't listed or the
/// digest differs.
async fn verify_sha256(dir: &Path, asset: &str) -> Result<()> {
    let sums = tokio::fs::read_to_string(dir.join("SHA256SUMS"))
        .await
        .context("reading SHA256SUMS")?;
    let want = sums
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hex = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            // Match on basename so a `sot-0.2.0-...` line matches regardless of
            // any leading path in the sums file.
            let base = Path::new(name).file_name()?.to_str()?;
            if base == asset {
                Some(hex.to_ascii_lowercase())
            } else {
                None
            }
        })
        .next()
        .ok_or_else(|| anyhow!("{asset} not listed in SHA256SUMS"))?;
    let bytes = tokio::fs::read(dir.join(asset))
        .await
        .with_context(|| format!("reading {asset} for verification"))?;
    let got = sha256_hex(&bytes);
    if got != want {
        bail!("sha256 mismatch for {asset}: expected {want}, got {got}");
    }
    Ok(())
}

/// Lowercase hex sha256 of `data`. Self-contained FIPS-180-4 implementation so
/// the updater carries no crypto dependency (verified by a known-answer test).
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pad: append 0x80, then zeros, then the 64-bit big-endian bit length.
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, wi) in w.iter_mut().enumerate().take(16) {
            let j = i * 4;
            *wi = u32::from_be_bytes([chunk[j], chunk[j + 1], chunk[j + 2], chunk[j + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = String::with_capacity(64);
    for word in h {
        out.push_str(&format!("{word:08x}"));
    }
    out
}

// ─── Unpack ─────────────────────────────────────────────────────────────

/// Extract a staged archive in place. `.tar.gz` via `tar`; `.zip` via
/// PowerShell `Expand-Archive` on Windows, `unzip` elsewhere. Apply/restart is
/// out of scope (ADR 0017 supervisor / systemd) — this only lays the bits down.
async fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        run_ok(
            tokio::process::Command::new("tar").args([
                "-xzf",
                &archive.to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ]),
        )
        .await
        .context("tar extract")
    } else if name.ends_with(".zip") {
        #[cfg(windows)]
        {
            run_ok(tokio::process::Command::new("powershell").args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Expand-Archive -Force -LiteralPath '{}' -DestinationPath '{}'",
                    archive.display(),
                    dest.display()
                ),
            ]))
            .await
            .context("Expand-Archive")
        }
        #[cfg(not(windows))]
        {
            run_ok(tokio::process::Command::new("unzip").args([
                "-o",
                &archive.to_string_lossy(),
                "-d",
                &dest.to_string_lossy(),
            ]))
            .await
            .context("unzip")
        }
    } else {
        bail!("don't know how to unpack {name}")
    }
}

/// Run a command to completion, erroring on a nonzero exit (with stderr).
async fn run_ok(cmd: &mut tokio::process::Command) -> Result<()> {
    let out = cmd.output().await.context("spawning")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("exit {:?}: {}", out.status.code(), stderr.trim());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_core_ordering() {
        assert_eq!(compare_versions("0.2.0", "0.2.0"), Ordering::Equal);
        assert_eq!(compare_versions("0.2.1", "0.2.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.2.0", "0.2.1"), Ordering::Less);
        assert_eq!(compare_versions("0.3.0", "0.2.9"), Ordering::Greater);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), Ordering::Greater);
        assert_eq!(compare_versions("0.10.0", "0.9.0"), Ordering::Greater); // numeric, not lexical
    }

    #[test]
    fn semver_v_prefix_and_whitespace() {
        assert_eq!(compare_versions("v0.3.0", "0.2.0"), Ordering::Greater);
        assert_eq!(compare_versions(" v0.2.0 ", "0.2.0"), Ordering::Equal);
    }

    #[test]
    fn semver_prerelease_ordering() {
        // A pre-release is lower than its release.
        assert_eq!(compare_versions("1.0.0-alpha", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-rc.1"), Ordering::Greater);
        // Identifier comparison, left to right.
        assert_eq!(compare_versions("1.0.0-alpha", "1.0.0-beta"), Ordering::Less);
        assert_eq!(
            compare_versions("1.0.0-alpha.1", "1.0.0-alpha"),
            Ordering::Greater // more identifiers on a common prefix
        );
        // Numeric identifiers rank below alphanumeric ones.
        assert_eq!(compare_versions("1.0.0-1", "1.0.0-alpha"), Ordering::Less);
        // Numeric identifiers compare as integers, not strings.
        assert_eq!(
            compare_versions("1.0.0-alpha.2", "1.0.0-alpha.10"),
            Ordering::Less
        );
    }

    #[test]
    fn semver_dev_marker_is_stripped_to_base() {
        // A `-dev+<sha>` running build compares by its base X.Y.Z against a
        // real release: 0.3.0 > 0.2.0-dev, and 0.2.0 release > 0.2.0-dev.
        assert_eq!(
            compare_versions("0.3.0", "0.2.0-dev+abc1234"),
            Ordering::Greater
        );
        assert_eq!(
            compare_versions("0.2.0", "0.2.0-dev+abc1234"),
            Ordering::Greater
        );
        // Build metadata does not affect ordering.
        assert_eq!(
            compare_versions("0.2.0-dev+aaa", "0.2.0-dev+bbb"),
            Ordering::Equal
        );
    }

    #[test]
    fn semver_unparsable_never_wins() {
        // A garbage "latest" must never read as an available update.
        assert_eq!(compare_versions("garbage", "0.2.0"), Ordering::Less);
        assert_eq!(compare_versions("0.2", "0.2.0"), Ordering::Less); // two components → unparsable
        assert_eq!(compare_versions("0.2.0.1", "0.2.0"), Ordering::Less); // four → unparsable
        assert_eq!(compare_versions("junk", "also-junk"), Ordering::Equal);
    }

    #[test]
    fn asset_name_derivation_per_triple() {
        assert_eq!(
            asset_name_for("0.2.0", "linux", "x86_64").as_deref(),
            Some("sot-0.2.0-linux-x86_64.tar.gz")
        );
        assert_eq!(
            asset_name_for("0.2.0", "windows", "x86_64").as_deref(),
            Some("sot-0.2.0-windows-x86_64.zip")
        );
        assert_eq!(
            asset_name_for("0.2.0", "macos", "aarch64").as_deref(),
            Some("sot-0.2.0-macos-aarch64.tar.gz")
        );
        // Off-matrix triples yield no asset (ADR 0030 §3).
        assert_eq!(asset_name_for("0.2.0", "macos", "x86_64"), None);
        assert_eq!(asset_name_for("0.2.0", "linux", "aarch64"), None);
        assert_eq!(asset_name_for("0.2.0", "freebsd", "x86_64"), None);
        // Version string is interpolated verbatim (pre-release tags included).
        assert_eq!(
            asset_name_for("0.3.0-rc.1", "linux", "x86_64").as_deref(),
            Some("sot-0.3.0-rc.1-linux-x86_64.tar.gz")
        );
    }

    #[test]
    fn platform_asset_matches_this_build() {
        // Whatever this test binary's triple is, platform_asset must agree with
        // the direct derivation (or both be None on an off-matrix triple).
        assert_eq!(
            platform_asset("0.2.0"),
            asset_name_for("0.2.0", TARGET_OS, TARGET_ARCH)
        );
    }

    #[test]
    fn sha256_known_answers() {
        // FIPS-180-4 / NIST test vectors.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        // Multi-block (>55 bytes forces a second padded block).
        assert_eq!(
            sha256_hex(&[0x61u8; 1000]), // 1000 'a's
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
        );
    }

    #[test]
    fn strip_v_only_strips_leading_v() {
        assert_eq!(strip_v("v0.2.0"), "0.2.0");
        assert_eq!(strip_v("0.2.0"), "0.2.0");
        assert_eq!(strip_v("version"), "ersion"); // documents the single-char strip
    }

    #[test]
    fn mode_parsing_from_env() {
        // Guard against races with other env-touching tests via a serialized
        // section: these are cheap and single-threaded within this fn.
        std::env::set_var("SOT_UPDATE_MODE", "off");
        assert_eq!(mode_from_env(), Mode::Off);
        std::env::set_var("SOT_UPDATE_MODE", "notify");
        assert_eq!(mode_from_env(), Mode::Notify);
        std::env::set_var("SOT_UPDATE_MODE", "auto"); // reserved → notify
        assert_eq!(mode_from_env(), Mode::Notify);
        std::env::set_var("SOT_UPDATE_MODE", "bogus"); // unknown → notify
        assert_eq!(mode_from_env(), Mode::Notify);
        std::env::remove_var("SOT_UPDATE_MODE"); // unset → notify (default)
        assert_eq!(mode_from_env(), Mode::Notify);
    }
}
