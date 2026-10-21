//! The three boot migrations of the row tomls: legacy unsuffixed state dirs, and the Windows legacy config dir.

use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use anyhow::{Context, Result};

use super::{app_config_dir, declared_host};

/// One-time migration: rename the legacy UNSUFFIXED state dirs to this
/// host's suffixed ones. Runs at daemon boot (from `load_all`, after
/// `migrate_legacy_windows_config_dir` on Windows); the first post-deploy
/// boot on the cohort inherits the legacy state (that's the primary dev box
/// — the only daemon that ever wrote it), every other host starts fresh, and
/// public single-home installs have nothing to migrate. Rename failures warn
/// and leave the legacy dir in place (nothing is destroyed).
///
/// The plain rename only covers "suffixed sibling absent yet". When it's
/// already THERE, this folds instead of skipping (`fold_unsuffixed_into_per_host`)
/// — field defect (2026-09-04): on Windows, `migrate_legacy_windows_config_dir`
/// can itself deposit an un-suffixed `workspaces`/`sessions` at the new root
/// — a SECONDARY legacy candidate's un-suffixed dir merges in verbatim-named
/// (see that function's secondary-candidate doc) — and if the PRIMARY
/// candidate (or an earlier boot's default-workspace write) already produced
/// the suffixed sibling at the new root first, the plain rename below used to
/// be a no-op: the secondary's rows sat un-suffixed at the new root forever,
/// in a directory `workspaces_dir()`/`sessions_dir()` never scans. A box that
/// already ran that broken migration has the identical shape stranded at the
/// new root from a prior boot — same fold, no separate recovery path, so
/// there is exactly one place that ever reads or writes an un-suffixed
/// registry dir under `app_config_dir()`.
pub(crate) fn migrate_legacy_state_dirs() {
    for name in ["workspaces", "sessions"] {
        let legacy = app_config_dir().join(name);
        let per_host = app_config_dir().join(format!("{name}-{}", declared_host()));
        if !legacy.is_dir() {
            continue;
        }
        if !per_host.exists() {
            match std::fs::rename(&legacy, &per_host) {
                Ok(()) => tracing::info!(from = %legacy.display(), to = %per_host.display(),
                    "migrated legacy state dir to per-host (ADR 0013/0014 addendum)"),
                Err(e) => tracing::warn!(error = %e, from = %legacy.display(),
                    "legacy state dir migration failed — leaving in place"),
            }
            continue;
        }
        fold_unsuffixed_into_per_host(&legacy, &per_host);
    }
}

/// Fold every `.toml` sitting directly in `legacy` (an un-suffixed registry
/// dir) into `per_host` (its host-suffixed sibling, which already exists),
/// then remove `legacy` once it's empty. Same non-colliding merge rule as
/// `merge_secondary_legacy_windows_children`: a name already present at the
/// destination wins outright — that file is left at `legacy`, untouched, and
/// only warned about (naming both paths); nothing at `per_host` is ever
/// overwritten. Unlike that function, this one MOVES (not copies) each
/// non-colliding file and deletes the source directory when empty — `legacy`
/// here is a dir this daemon owns under its OWN new root, not an
/// still-independently-live legacy candidate root that must be left exactly
/// as found either way. Logs one line per moved toml (field-log
/// distinguishability, matching `migrate_legacy_windows_config_dir`'s own
/// convention). Best-effort: an unreadable `legacy` dir, or a failed
/// individual rename, is a warning, not a boot error — the daemon still
/// starts against whatever `per_host` alone resolves to.
fn fold_unsuffixed_into_per_host(legacy: &Path, per_host: &Path) {
    let entries = match std::fs::read_dir(legacy) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(error = %e, dir = %legacy.display(),
                "could not read a stranded un-suffixed state dir to fold it into the per-host one");
            return;
        }
    };
    // Counts anything left behind at `legacy` — a name collision, a failed
    // rename, or a non-toml entry — so `legacy` is only removed once it's
    // genuinely empty, never a non-empty dir masquerading as folded.
    let mut left_behind = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("toml") {
            left_behind += 1;
            continue;
        }
        let Some(name) = path.file_name() else {
            left_behind += 1;
            continue;
        };
        let dst = per_host.join(name);
        if dst.exists() {
            tracing::warn!(from = %path.display(), to = %dst.display(),
                "a stranded un-suffixed registry has a toml with the same name as one \
                 already at the per-host dir — left in place, not folded (rename by hand to adopt it)");
            left_behind += 1;
            continue;
        }
        match std::fs::rename(&path, &dst) {
            Ok(()) => tracing::info!(from = %path.display(), to = %dst.display(),
                "folded a stranded un-suffixed registry toml into the per-host dir"),
            Err(e) => {
                tracing::warn!(error = %e, from = %path.display(), to = %dst.display(),
                    "could not fold a stranded un-suffixed registry toml into the per-host dir");
                left_behind += 1;
            }
        }
    }
    if left_behind == 0 {
        if let Err(e) = std::fs::remove_dir(legacy) {
            tracing::warn!(error = %e, dir = %legacy.display(),
                "folded every toml out of a stranded un-suffixed registry dir but could not remove it");
        }
    }
}

/// Directory entries directly under `root` matching the backend's OWN
/// registry dirs: `workspaces`, `sessions` (pre-per-host legacy shape) and
/// `workspaces-*`/`sessions-*` (current per-host shape, any host suffix —
/// `declared_host()` isn't consulted here since migration runs before/
/// independent of which host string this boot resolves). Named
/// explicitly rather than swept wholesale because `<...>\.config\sot` is
/// NOT backend-exclusive on Windows: the frontend resolves its OWN files
/// there too (`settings.toml`, `keybindings.toml`, `hosts.toml`,
/// `state-<host>.toml`) — renaming the whole dir would silently reset the
/// frontend (Codex review finding). Sorted for deterministic iteration
/// order (used by both the rename loop and tests).
#[cfg(windows)]
fn backend_registry_children(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else { continue };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "workspaces"
            || name == "sessions"
            || name.starts_with("workspaces-")
            || name.starts_with("sessions-")
        {
            out.push(name.to_string());
        }
    }
    out.sort();
    out
}

/// Every root `app_config_dir()` (or its pre-fix predecessor) could have
/// resolved to on Windows, in the order this function probes them —
/// deduplicated by value (`HOME` and `USERPROFILE` commonly hold the same
/// path under Git for Windows; the current-drive and `%SystemDrive%` `tmp`
/// candidates likewise collapse when they're the same drive), so a value
/// reachable by two different env vars is probed, and migrated, exactly
/// once:
///   1. `<XDG_CONFIG_HOME>\sot` — `config_dir()`'s own first tier, honoured
///      on every platform pre-fix, including Windows.
///   2. `<HOME>\.config\sot` — a git-bash shell exports `HOME`; its value
///      there IS `%USERPROFILE%` (so this and #3 usually collapse to one
///      candidate after dedup, but not always — `HOME` can be overridden).
///   3. `<USERPROFILE>\.config\sot` — same shape, keyed off the env var a
///      real Windows login always sets (unlike `HOME`), so migration finds
///      the right root regardless of which shell THIS boot happens to be
///      launched from.
///   4. `\tmp\.config\sot` (current-drive-relative-root) — `config_dir()`'s
///      literal fallback when neither `XDG_CONFIG_HOME` nor `HOME` was
///      set; Windows roots a leading `/`/`\` with no drive prefix onto
///      whatever the process's current drive is.
///   5. `<SystemDrive>\tmp\.config\sot` — the same fallback, explicit-
///      drive form, for when the daemon's current drive isn't the system
///      drive (field-observed value on the box that WAS on the system
///      drive: `wrote backend identity toml
///      toml="/tmp/.config\sot\sessions-<host>\local.toml"`, i.e.
///      `C:\tmp\.config\sot\...`).
#[cfg(windows)]
fn legacy_windows_config_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(v) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(v).join("sot"));
    }
    if let Some(v) = std::env::var_os("HOME") {
        out.push(PathBuf::from(v).join(".config").join("sot"));
    }
    if let Some(v) = std::env::var_os("USERPROFILE") {
        out.push(PathBuf::from(v).join(".config").join("sot"));
    }
    // `config_dir()`'s own literal fallback, byte-for-byte — Windows roots
    // a leading separator with no drive prefix onto the current drive.
    out.push(PathBuf::from("/tmp/.config").join("sot"));
    // Built by string formatting, not `PathBuf::join` — `%SystemDrive%`'s
    // value is a bare `C:` with no trailing separator, and joining onto a
    // prefix-only `PathBuf` with no root component produces a
    // DRIVE-RELATIVE path (`C:tmp\...`, relative to that drive's current
    // dir) rather than the absolute `C:\tmp\...` intended here.
    let system_drive = std::env::var_os("SystemDrive")
        .and_then(|v| v.into_string().ok())
        .unwrap_or_else(|| "C:".to_string());
    out.push(PathBuf::from(format!("{system_drive}\\tmp\\.config\\sot")));

    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// Windows-only, one-time migration, run at boot before `scan_disk` reads
/// the registry: before this fix, `app_config_dir()` fell straight through
/// to `config_dir()` above with no Windows branch, so the registry root
/// depended on which shell launched the daemon (see `app_config_dir`'s doc
/// for the field report this fixes) — see `legacy_windows_config_candidates`
/// for every root that could have produced.
///
/// Invariant: **a box's legacy registry moves exactly once, into the
/// canonical daemon.** The "already migrated?" check below (empty
/// `new_root`) alone can't hold it — `new_root` comes from THIS process's
/// own overridable env, so a scratch daemon sees it empty too and would
/// `rename` the box's real registry into itself (field-proven). Fix lives
/// one level up: `scan_disk`'s `adopt_legacy_registry` gate (see its doc).
///
/// Moves ONLY the backend's own registry children
/// (`backend_registry_children`) from the FIRST candidate that has any,
/// via one `rename` per child — never the whole legacy dir, which also
/// holds frontend-owned files on Windows. For every OTHER candidate that
/// also has backend children: each `.toml` whose file name doesn't already
/// exist at the destination is COPIED in (not moved); a name that DOES
/// collide is left at the source and only warned about (naming both
/// paths) — no merge, no overwrite, no deletion anywhere in this function.
///
/// "Already migrated" is decided by the PRESENCE of backend children under
/// the new root, not by the root existing — `app_config_dir()`'s directory
/// can already exist empty (default-workspace persistence creates it) on a
/// box with nothing to migrate, and treating that as "done" would silently
/// skip real legacy data sitting in a candidate.
///
/// A failed rename of a primary child is a BOOT ERROR (`Err`, naming both
/// paths) rather than a warning: with the data only partially moved, this
/// must refuse to boot rather than silently read whatever now sits at the
/// new root (empty or partial) as if it were the whole registry — no
/// fallback to reading the old location, no seeding a fresh registry
/// beside a stranded one. Secondary-candidate copy failures stay
/// warn-only: that candidate's data is untouched (still at its original
/// path) either way, so there's nothing to strand.
///
/// Logs one INFO line per probed candidate (`legacy registry probe: <path>
/// — found (migrating)` / `— empty` / `— absent`) plus one summary line
/// when the new root already had backend children ("migration not
/// needed") — before this, a field boot log with nothing to migrate was
/// silent here, so it couldn't be told apart from "this code never ran"
/// (v0.6.0-rc.4 field report). Returns the per-candidate outcomes so tests
/// can assert on what was probed without a tracing test-capture harness
/// (this crate has none); behaviour is otherwise unchanged.
#[cfg(windows)]
pub(super) fn migrate_legacy_windows_config_dir() -> Result<Vec<(PathBuf, ProbeOutcome)>> {
    let new_root = app_config_dir();
    if !backend_registry_children(&new_root).is_empty() {
        tracing::info!(root = %new_root.display(),
            "legacy registry migration not needed: new root already has backend children");
        return Ok(Vec::new());
    }
    let candidates = legacy_windows_config_candidates();
    let mut primary_done = false;
    let mut probes = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let (children, outcome) = probe_candidate(candidate);
        match outcome {
            ProbeOutcome::Found => {
                tracing::info!("legacy registry probe: {} — found (migrating)", candidate.display())
            }
            ProbeOutcome::Empty => {
                tracing::info!("legacy registry probe: {} — empty", candidate.display())
            }
            ProbeOutcome::Absent => {
                tracing::info!("legacy registry probe: {} — absent", candidate.display())
            }
        }
        probes.push((candidate.clone(), outcome));
        if children.is_empty() {
            continue;
        }
        if !primary_done {
            std::fs::create_dir_all(&new_root).with_context(|| {
                format!("create Windows config root {}", new_root.display())
            })?;
            for child in &children {
                let src = candidate.join(child);
                let dst = new_root.join(child);
                std::fs::rename(&src, &dst).with_context(|| {
                    format!(
                        "migrate legacy Windows config dir: moving {} to {} failed \
                         (registry left split across both paths)",
                        src.display(),
                        dst.display()
                    )
                })?;
                tracing::info!(from = %src.display(), to = %dst.display(),
                    "migrated legacy Windows registry child");
            }
            primary_done = true;
        } else {
            merge_secondary_legacy_windows_children(candidate, &children, &new_root);
        }
    }
    Ok(probes)
}

/// Outcome of probing one `legacy_windows_config_candidates()` entry —
/// logged and returned by `migrate_legacy_windows_config_dir` so a boot
/// with nothing to migrate is distinguishable, in the log and in tests,
/// from this code never having run.
#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProbeOutcome {
    /// The candidate directory exists and has backend registry children.
    Found,
    /// The candidate directory exists but has no backend registry children.
    Empty,
    /// The candidate directory doesn't exist.
    Absent,
}

#[cfg(windows)]
fn probe_candidate(candidate: &Path) -> (Vec<String>, ProbeOutcome) {
    if !candidate.is_dir() {
        return (Vec::new(), ProbeOutcome::Absent);
    }
    let children = backend_registry_children(candidate);
    let outcome = if children.is_empty() { ProbeOutcome::Empty } else { ProbeOutcome::Found };
    (children, outcome)
}

/// Best-effort merge for a legacy Windows config root OTHER than the one
/// adopted as primary (see `migrate_legacy_windows_config_dir`): for each
/// backend child directory it also has, copy in any `.toml` whose file
/// name isn't already present at the destination; warn (naming both
/// paths) and skip any that is. Never touches or removes the source —
/// this candidate is left exactly as it was found either way.
#[cfg(windows)]
fn merge_secondary_legacy_windows_children(candidate: &Path, children: &[String], new_root: &Path) {
    for child in children {
        let src_dir = candidate.join(child);
        let dst_dir = new_root.join(child);
        if let Err(e) = std::fs::create_dir_all(&dst_dir) {
            tracing::warn!(error = %e, dir = %dst_dir.display(),
                "could not create dir to merge a secondary legacy Windows registry child into — skipping");
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&src_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("toml") {
                continue;
            }
            let Some(name) = path.file_name() else { continue };
            let dst_file = dst_dir.join(name);
            if dst_file.exists() {
                tracing::warn!(from = %path.display(), to = %dst_file.display(),
                    "a second legacy Windows registry has a toml with the same name as one \
                     already migrated — left in place, not merged (rename by hand to adopt it)");
                continue;
            }
            #[allow(clippy::disallowed_methods, reason = "a data copy of a toml, no program is run from it")]
            if let Err(e) = std::fs::copy(&path, &dst_file) {
                tracing::warn!(error = %e, from = %path.display(), to = %dst_file.display(),
                    "could not copy a non-colliding toml from a secondary legacy Windows registry");
            }
        }
    }
}

#[cfg(all(test, windows))]
#[path = "migrate_tests.rs"]
mod tests;
