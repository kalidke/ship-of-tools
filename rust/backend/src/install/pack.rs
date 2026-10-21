//! The pack a run installs: the folder the bootstrap verified, holding `SHA256SUMS`, the archive for this system, `COMMIT`
//! (and `LIBC` from a Linux pack of the line) and the archive unpacked beside them, `sot-<version>-<target>/`. Its
//! identity is read here and each part checked against the others, so a half-written or mislabelled pack installs
//! nothing (scripts/CLAUDE.md, Installers: "Verified before it runs").

use std::path::{Path, PathBuf};

use super::cmd;

#[derive(Debug, Clone)]
pub(super) struct Pack {
    /// The folder `SHA256SUMS` is in.
    pub dir: PathBuf,
    /// The version the archive's name and its binaries carry, with no `v`.
    pub version: String,
    /// The 40-digit commit the binaries were built from.
    pub commit: String,
    /// The unpacked folder: the binaries, and on Unix `sot-apply` and the unit template.
    pub bin: PathBuf,
    /// The libc a Linux pack of the line says its daemon targets.
    #[cfg_attr(windows, allow(dead_code, reason = "a Linux pack of the line says its libc"))]
    pub libc: Option<String>,
}

impl Pack {
    pub fn tag(&self) -> String {
        format!("v{}", self.version)
    }

    pub fn short(&self) -> &str {
        &self.commit[..9]
    }

    /// A binary of the pack by its name (`sotd`, `sot`, `sot-capsule`), `.exe` on Windows.
    pub fn exe(&self, name: &str) -> PathBuf {
        self.bin.join(exe_name(name))
    }
}

pub(super) fn exe_name(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// The version an archive name carries for `target`, from a `SHA256SUMS` text: exactly one `sot-<version>-<target>.<ext>`
/// listed, its version spelled with `[0-9A-Za-z.+-]` only (it becomes a folder name and a git ref).
pub(super) fn archive_version(sums: &[sot_updater::fetch::sums::SumEntry], target: &str) -> Result<String, String> {
    let versions: Vec<String> = sums
        .iter()
        .filter_map(|e| sot_updater::platform::parse_asset_name(&e.name))
        .filter(|(_, plat)| *plat == target)
        .map(|(v, _)| v)
        .collect();
    let [version] = versions.as_slice() else {
        return Err(format!("SHA256SUMS must name exactly one sot-<version>-{target} archive (it names {})", versions.len()));
    };
    if !version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-')) {
        return Err(format!("unsafe version '{version}' in SHA256SUMS"));
    }
    Ok(version.clone())
}

/// Whether a binary's `--version` line says `<name> <version>` built at `commit`: `<name> <version> (<sha> <date>, ...)`
/// with the sha a prefix of the commit, or `<name> <version> (protocol N)` from a build that names no commit.
pub(super) fn binary_says(line: &str, name: &str, version: &str, commit: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix(&format!("{name} {version} (")) else { return false };
    if rest.starts_with("protocol ") {
        return true;
    }
    let sha = rest.split([' ', ',', ')']).next().unwrap_or("");
    sha.len() >= 7 && commit.starts_with(sha)
}

/// Read and check the pack in `dir` for `target` (`linux-x86_64`, ...): its identity and its `sotd`. `want` is the tag
/// the run was told to install, when it was told one.
pub(super) fn read(dir: &Path, target: &str, want: Option<&str>) -> Result<Pack, String> {
    let sums_path = dir.join("SHA256SUMS");
    let text = std::fs::read_to_string(&sums_path).map_err(|e| format!("{}: {e}", sums_path.display()))?;
    let sums = sot_updater::fetch::sums::parse_sums(&text).map_err(|e| format!("{}: {e:#}", sums_path.display()))?;
    let version = archive_version(&sums, target)?;
    if let Some(want) = want {
        if sot_updater::semver::strip_v(want) != version {
            return Err(format!("{} names {version}, not {want}", sums_path.display()));
        }
    }
    let archive = sums
        .iter()
        .find(|e| sot_updater::platform::parse_asset_name(&e.name).is_some_and(|(v, p)| v == version && p == target))
        .map(|e| e.name.clone())
        .unwrap_or_default();
    for name in [archive.as_str(), "COMMIT"] {
        let digest = sot_updater::fetch::sums::lookup(&sums, name).map_err(|_| format!("{} does not cover {name}", sums_path.display()))?;
        let bytes = std::fs::read(dir.join(name)).map_err(|e| format!("{} has no {name}: {e}", dir.display()))?;
        let got = sot_updater::fetch::hash::sha256_bytes(&bytes);
        if got != digest {
            return Err(format!("{name} does not match its SHA256SUMS entry (got {got})"));
        }
    }
    let commit = std::fs::read_to_string(dir.join("COMMIT")).unwrap_or_default().trim().to_string();
    if commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(format!("{}/COMMIT is not a 40-digit commit id", dir.display()));
    }
    let bin = dir.join(format!("sot-{version}-{target}"));
    let libc = std::fs::read_to_string(dir.join("LIBC")).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let pack = Pack { dir: dir.to_path_buf(), version, commit, bin, libc };
    // The frontend is asked by the role that runs it: a backend-only host may lack the libc it was built for.
    pack.check_binary("sotd")?;
    Ok(pack)
}

impl Pack {
    /// Whether the pack's binary `name` runs here and says the pack's version at its commit; why not, when not.
    pub fn check_binary(&self, name: &str) -> Result<(), String> {
        let exe = self.exe(name);
        let ran = cmd::output(std::process::Command::new(&exe).arg("--version"));
        let line = ran.first_line();
        if ran.ok() && binary_says(&line, name, &self.version, &self.commit) {
            return Ok(());
        }
        let said = if ran.ok() { format!("says '{line}'") } else { format!("does not run here ({})", ran.last_said()) };
        Err(format!("{} {said}, which does not match {} at {} (its SHA256SUMS and COMMIT)", exe.display(), self.version, self.short()))
    }
}

#[cfg(test)]
#[path = "pack_tests.rs"]
mod tests;
