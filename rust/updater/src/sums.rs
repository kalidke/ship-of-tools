//! `SHA256SUMS` parsing and release discovery.
//!
//! The sums file doubles as the discovery document (Codex review, SHOULD-FIX
//! 1): `GET releases/latest/download/SHA256SUMS` is one documented request
//! that yields both the released version (derivable from the deterministic
//! `sot-<ver>-<platform>.<ext>` asset names it lists) and the digests needed
//! to verify the platform archive. No Releases API call, no rate limit, no
//! redirect-header grammar to depend on.

use anyhow::{anyhow, bail, Result};

use crate::identity::{validate_sha256, ReleaseIdentity};
use crate::platform::parse_asset_name;
use crate::semver::parse_semver;

/// One `<hex>  <name>` line. The two-space and ` *` (binary-marker) formats
/// both parse; names match on basename so a leading path in the sums file is
/// tolerated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumEntry {
    pub sha256: String,
    pub name: String,
}

/// Parse a SHA256SUMS document. Lines that don't look like sum entries are
/// ignored (empty lines, comments); a present-but-malformed digest fails loud.
pub fn parse_sums(text: &str) -> Result<Vec<SumEntry>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(hex), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        let name = name.trim_start_matches('*');
        let base = std::path::Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("unparsable name in SHA256SUMS line: {line:?}"))?;
        let hex = hex.to_ascii_lowercase();
        validate_sha256(&hex)?;
        out.push(SumEntry {
            sha256: hex,
            name: base.to_string(),
        });
    }
    if out.is_empty() {
        bail!("SHA256SUMS contains no sum entries");
    }
    Ok(out)
}

/// Digest for `asset` in a parsed sums list.
pub fn lookup(entries: &[SumEntry], asset: &str) -> Result<String> {
    entries
        .iter()
        .find(|e| e.name == asset)
        .map(|e| e.sha256.clone())
        .ok_or_else(|| anyhow!("{asset} not listed in SHA256SUMS"))
}

/// Discover the release a SHA256SUMS document describes, for one target
/// platform. Requires every `sot-*` archive entry to agree on a single valid
/// semver version (an inconsistent listing fails loud rather than guessing),
/// then pins the identity for `target` — or reports that the release doesn't
/// ship it.
pub fn discover(entries: &[SumEntry], repo: &str, target: &str) -> Result<ReleaseIdentity> {
    let mut version: Option<String> = None;
    let mut for_target: Option<SumEntry> = None;
    let mut saw_archives = false;
    for e in entries {
        let Some((v, plat)) = parse_asset_name(&e.name) else {
            continue; // non-archive release file (e.g. sotd.service copies)
        };
        // Only PRODUCT archives count: the version part must be strict
        // semver. A future companion asset that happens to match the
        // `sot-*-<platform>` shape (e.g. `sot-installer-1.0-…`) must be
        // skipped, not allowed to brick discovery for every deployed
        // updater — auto-update is the only channel that ships fixes.
        if parse_semver(&v).is_none() {
            tracing::debug!(asset = %e.name, "skipping non-product sot-* asset in SHA256SUMS");
            continue;
        }
        saw_archives = true;
        match &version {
            None => version = Some(v.clone()),
            Some(prev) if *prev != v => {
                bail!("SHA256SUMS mixes versions {prev} and {v} — refusing");
            }
            Some(_) => {}
        }
        if plat == target {
            for_target = Some(e.clone());
        }
    }
    if !saw_archives {
        bail!("SHA256SUMS lists no sot release archives");
    }
    let version = version.expect("saw_archives implies version");
    let entry = for_target
        .ok_or_else(|| anyhow!("release v{version} has no asset for platform {target}"))?;
    let id = ReleaseIdentity {
        repo: repo.to_string(),
        tag: format!("v{version}"),
        version,
        target: target.to_string(),
        asset: entry.name,
        asset_sha256: entry.sha256,
    };
    id.validate()?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-linux-x86_64.tar.gz
0202020202020202020202020202020202020202020202020202020202020202  sot-0.6.0-windows-x86_64.zip
0303030303030303030303030303030303030303030303030303030303030303 *sot-0.6.0-macos-aarch64.tar.gz
";

    #[test]
    fn parse_and_lookup() {
        let entries = parse_sums(SUMS).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(
            lookup(&entries, "sot-0.6.0-windows-x86_64.zip").unwrap(),
            "02".repeat(32)
        );
        assert!(lookup(&entries, "sot-0.6.0-linux-aarch64.tar.gz").is_err());
    }

    #[test]
    fn discover_pins_platform_identity() {
        let entries = parse_sums(SUMS).unwrap();
        let id = discover(&entries, "kalidke/ship-of-tools", "linux-x86_64").unwrap();
        assert_eq!(id.tag, "v0.6.0");
        assert_eq!(id.asset, "sot-0.6.0-linux-x86_64.tar.gz");
        assert_eq!(id.asset_sha256, "01".repeat(32));
        id.validate().unwrap();
    }

    #[test]
    fn discover_rejects_mixed_versions() {
        let mixed = format!(
            "{SUMS}0404040404040404040404040404040404040404040404040404040404040404  sot-0.7.0-linux-x86_64.tar.gz\n"
        );
        let entries = parse_sums(&mixed).unwrap();
        assert!(discover(&entries, "kalidke/ship-of-tools", "linux-x86_64").is_err());
    }

    #[test]
    fn discover_reports_missing_platform() {
        let linux_only = "\
0101010101010101010101010101010101010101010101010101010101010101  sot-0.6.0-linux-x86_64.tar.gz
";
        let entries = parse_sums(linux_only).unwrap();
        let err = discover(&entries, "kalidke/ship-of-tools", "macos-aarch64").unwrap_err();
        assert!(err.to_string().contains("no asset for platform"));
    }

    #[test]
    fn parse_rejects_bad_digest() {
        assert!(parse_sums("zz  sot-0.6.0-linux-x86_64.tar.gz\n").is_err());
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(parse_sums("\n# nothing\n").is_err());
    }
}
