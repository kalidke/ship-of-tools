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
            if let Err(e) = std::fs::copy(&path, &dst_file) {
                tracing::warn!(error = %e, from = %path.display(), to = %dst_file.display(),
                    "could not copy a non-colliding toml from a secondary legacy Windows registry");
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::rows::Workspaces;
    use crate::rows::store::scan_disk;
    use crate::rows::store::support_tests::env_guarded;

    /// Scratch roots for one migration test: `USERPROFILE`-, `SystemDrive`-
    /// and `XDG_CONFIG_HOME`-style dirs, plus a `LOCALAPPDATA`-style dir
    /// for the new root. All under one per-test base so a single
    /// `remove_dir_all` on the base cleans everything up.
    #[cfg(windows)]
    struct MigrationScratch {
        base: PathBuf,
        userprofile: PathBuf,
        system_drive: String,
        xdg_config_home: PathBuf,
        localappdata: PathBuf,
    }

    #[cfg(windows)]
    impl MigrationScratch {
        fn new(name: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let base = std::env::temp_dir().join(format!(
                "sot-ws-win-config-migrate-{}-{}-{name}",
                std::process::id(),
                n
            ));
            Self {
                userprofile: base.join("userprofile"),
                // No trailing separator, matching a real `%SystemDrive%`
                // value (`C:`) — the base dir's string form stands in for
                // the drive letter here.
                system_drive: base.join("sysdrive-root").to_string_lossy().into_owned(),
                xdg_config_home: base.join("xdg-config-home"),
                localappdata: base.join("localappdata"),
                base,
            }
        }

        fn userprofile_legacy(&self) -> PathBuf {
            self.userprofile.join(".config").join("sot")
        }

        fn system_drive_legacy(&self) -> PathBuf {
            PathBuf::from(format!("{}\\tmp\\.config\\sot", self.system_drive))
        }

        fn xdg_config_home_legacy(&self) -> PathBuf {
            self.xdg_config_home.join("sot")
        }

        fn new_root(&self) -> PathBuf {
            self.localappdata.join("sot").join("config")
        }

        /// Clears every candidate-relevant var, then sets only
        /// `USERPROFILE`/`SystemDrive`/`LOCALAPPDATA` (what a real Windows
        /// login always has) — a test that wants `XDG_CONFIG_HOME` or
        /// `HOME` in the mix sets it after calling this.
        fn apply_env(&self) {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
            std::env::set_var("USERPROFILE", &self.userprofile);
            std::env::set_var("SystemDrive", &self.system_drive);
            std::env::set_var("LOCALAPPDATA", &self.localappdata);
        }
    }

    #[cfg(windows)]
    impl Drop for MigrationScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    #[cfg(windows)]
    fn backend_registry_children_finds_workspace_and_session_dirs_not_frontend_files() {
        let s = MigrationScratch::new("children-scan");
        let root = s.base.join("scan-root");
        std::fs::create_dir_all(root.join("workspaces-host")).unwrap();
        std::fs::create_dir_all(root.join("sessions-otherhost")).unwrap();
        std::fs::create_dir_all(root.join("workspaces")).unwrap();
        std::fs::write(root.join("settings.toml"), "").unwrap();
        std::fs::write(root.join("hosts.toml"), "").unwrap();
        assert_eq!(
            backend_registry_children(&root),
            vec!["sessions-otherhost", "workspaces", "workspaces-host"]
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_moves_only_backend_children_and_leaves_frontend_files_in_place() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("frontend-untouched");
        let legacy = s.userprofile_legacy();
        let workspace_toml = legacy.join("workspaces-host").join("alpha.toml");
        std::fs::create_dir_all(workspace_toml.parent().unwrap()).unwrap();
        std::fs::write(&workspace_toml, "slug = \"alpha\"\n").unwrap();
        // Frontend-owned files sharing the same legacy root — must survive.
        std::fs::write(legacy.join("settings.toml"), "frontend settings").unwrap();
        std::fs::write(legacy.join("hosts.toml"), "frontend hosts").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("workspaces-host").exists(), "backend child should have moved");
        assert!(s.new_root().join("workspaces-host").join("alpha.toml").is_file());
        // Frontend files are untouched at their original path — the
        // legacy dir itself was never renamed, only its backend children.
        assert_eq!(
            std::fs::read_to_string(legacy.join("settings.toml")).unwrap(),
            "frontend settings"
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("hosts.toml")).unwrap(),
            "frontend hosts"
        );
    }

    /// Field defect: a scratch/test daemon must never drain the box's
    /// shared legacy registry into itself. Proves the gate at `scan_disk`.
    #[test]
    #[cfg(windows)]
    fn scan_disk_never_adopts_legacy_registry_without_the_canonical_flag() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("no-adopt-without-flag");
        let legacy = s.userprofile_legacy();
        let workspace_toml = legacy.join("workspaces-host").join("alpha.toml");
        std::fs::create_dir_all(workspace_toml.parent().unwrap()).unwrap();
        std::fs::write(&workspace_toml, "slug = \"alpha\"\n").unwrap();
        s.apply_env();

        scan_disk(&Workspaces::new(), false).unwrap();

        assert!(workspace_toml.is_file(), "legacy registry must be left exactly where it was");
        assert!(!s.new_root().join("workspaces-host").exists(), "nothing may land at the new root");
    }

    #[test]
    #[cfg(windows)]
    fn migrate_prefers_xdg_config_home_over_every_other_candidate() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("xdg-first");
        s.apply_env();
        std::env::set_var("XDG_CONFIG_HOME", &s.xdg_config_home);
        let xdg_legacy = s.xdg_config_home_legacy();
        std::fs::create_dir_all(xdg_legacy.join("sessions-host")).unwrap();
        // A USERPROFILE-rooted candidate also exists, but XDG_CONFIG_HOME
        // must win since it's probed first.
        std::fs::create_dir_all(s.userprofile_legacy().join("sessions-host")).unwrap();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!xdg_legacy.join("sessions-host").exists());
        assert!(s.new_root().join("sessions-host").is_dir());
        // The USERPROFILE candidate was only SECONDARY here — its own
        // empty child dir is left exactly as found, not deleted.
        assert!(s.userprofile_legacy().join("sessions-host").is_dir());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_falls_back_to_system_drive_root_when_nothing_earlier_has_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("sysdrive");
        let legacy = s.system_drive_legacy();
        std::fs::create_dir_all(legacy.join("sessions-host")).unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("sessions-host").exists());
        assert!(s.new_root().join("sessions-host").is_dir());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_copies_noncolliding_tomls_from_a_secondary_candidate_and_warns_on_collision() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("secondary-merge");
        let primary = s.userprofile_legacy();
        std::fs::create_dir_all(primary.join("workspaces-host")).unwrap();
        std::fs::write(primary.join("workspaces-host").join("alpha.toml"), "primary alpha").unwrap();
        let secondary = s.system_drive_legacy();
        std::fs::create_dir_all(secondary.join("workspaces-host")).unwrap();
        // Non-colliding: gets copied in.
        std::fs::write(secondary.join("workspaces-host").join("beta.toml"), "secondary beta").unwrap();
        // Colliding name: left at the source, not overwritten at the dest.
        std::fs::write(secondary.join("workspaces-host").join("alpha.toml"), "secondary alpha").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        let dst = s.new_root().join("workspaces-host");
        assert_eq!(std::fs::read_to_string(dst.join("alpha.toml")).unwrap(), "primary alpha");
        assert_eq!(std::fs::read_to_string(dst.join("beta.toml")).unwrap(), "secondary beta");
        // Secondary candidate is left in place entirely — including the
        // colliding file, which was never deleted or overwritten.
        assert_eq!(
            std::fs::read_to_string(secondary.join("workspaces-host").join("alpha.toml")).unwrap(),
            "secondary alpha"
        );
    }

    #[test]
    #[cfg(windows)]
    fn migrate_no_op_when_new_root_already_has_backend_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("already-migrated");
        let legacy = s.userprofile_legacy();
        std::fs::create_dir_all(legacy.join("workspaces-host")).unwrap();
        std::fs::write(legacy.join("workspaces-host").join("stray.toml"), "should not move").unwrap();
        std::fs::create_dir_all(s.new_root().join("workspaces-host")).unwrap();
        std::fs::write(s.new_root().join("workspaces-host").join("canonical.toml"), "canonical").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(legacy.join("workspaces-host").join("stray.toml").is_file());
        assert!(s.new_root().join("workspaces-host").join("canonical.toml").is_file());
        assert!(!s.new_root().join("workspaces-host").join("stray.toml").exists());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_proceeds_when_new_root_exists_but_has_no_backend_children_yet() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("empty-new-root");
        let legacy = s.userprofile_legacy();
        std::fs::create_dir_all(legacy.join("workspaces-host")).unwrap();
        std::fs::write(legacy.join("workspaces-host").join("alpha.toml"), "alpha").unwrap();
        // The new root dir already exists (e.g. default-workspace
        // persistence created it) but holds no backend children yet — this
        // must NOT be mistaken for "already migrated".
        std::fs::create_dir_all(s.new_root()).unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!legacy.join("workspaces-host").exists());
        assert!(s.new_root().join("workspaces-host").join("alpha.toml").is_file());
    }

    #[test]
    #[cfg(windows)]
    fn migrate_no_op_when_no_candidate_has_backend_children() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("no-legacy");
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();

        assert!(!s.new_root().exists());
    }

    /// The daemon logs one `tracing::info!` line per probed candidate
    /// (found/empty/absent) so a field boot log can tell "probed, nothing
    /// there" apart from "this code never ran" — but this crate has no
    /// tracing test-capture harness, so this asserts on the return value
    /// (`Vec<(PathBuf, ProbeOutcome)>`) the same probe loop reports
    /// through, rather than on captured log output.
    #[test]
    #[cfg(windows)]
    fn migrate_returns_probe_outcome_per_candidate_including_the_empty_case() {
        let _guard = env_guarded();
        let s = MigrationScratch::new("probe-outcomes");
        // USERPROFILE candidate: directory exists but has no backend
        // registry children — "empty", the case a silent log can't be
        // told apart from "absent" or "never probed".
        std::fs::create_dir_all(s.userprofile_legacy()).unwrap();
        // SystemDrive candidate: has backend children — "found".
        std::fs::create_dir_all(s.system_drive_legacy().join("workspaces-host")).unwrap();
        s.apply_env();

        let probes = migrate_legacy_windows_config_dir().unwrap();

        let outcome_for = |path: &Path| {
            probes.iter().find(|(p, _)| p == path).map(|(_, o)| *o)
        };
        assert_eq!(outcome_for(&s.userprofile_legacy()), Some(ProbeOutcome::Empty));
        assert_eq!(outcome_for(&s.system_drive_legacy()), Some(ProbeOutcome::Found));
    }

    /// End-to-end field defect (2026-09-04): a PRIMARY legacy candidate
    /// already had the host-suffixed dir (an earlier boot at that root had
    /// already run the per-host migration), while a SECONDARY candidate
    /// still had an un-suffixed one holding a row the primary never saw.
    /// `migrate_legacy_windows_config_dir` moves the primary's suffixed dir
    /// straight across (name preserved) and merges the secondary's
    /// un-suffixed one in verbatim-named — landing an un-suffixed
    /// `workspaces` at the new root right next to the suffixed one.
    /// `migrate_legacy_state_dirs`, called right after (mirroring
    /// `scan_disk`'s own call order), used to see the suffixed sibling
    /// already present and stop, stranding the secondary's row where the
    /// daemon's own `workspaces_dir()` never scans. This proves both the
    /// stranding (mid-run) and the fold that now recovers it.
    #[test]
    #[cfg(windows)]
    fn migrate_then_fold_lands_every_legacy_row_in_the_host_suffixed_dir() {
        let _guard = env_guarded();
        std::env::set_var("SOT_SELF_HOST", "host");
        let s = MigrationScratch::new("migrate-then-fold");
        let primary = s.userprofile_legacy();
        std::fs::create_dir_all(primary.join("workspaces-host")).unwrap();
        std::fs::write(primary.join("workspaces-host").join("alpha.toml"), "alpha").unwrap();
        let secondary = s.system_drive_legacy();
        std::fs::create_dir_all(secondary.join("workspaces")).unwrap();
        std::fs::write(secondary.join("workspaces").join("beta.toml"), "beta").unwrap();
        s.apply_env();

        migrate_legacy_windows_config_dir().unwrap();
        // Confirms the setup reproduces the stranding, not just the fix:
        // beta landed un-suffixed at the new root, next to workspaces-host.
        assert!(s.new_root().join("workspaces").join("beta.toml").is_file());

        migrate_legacy_state_dirs();

        let dst = s.new_root().join("workspaces-host");
        assert_eq!(
            std::fs::read_to_string(dst.join("alpha.toml")).unwrap(),
            "alpha"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("beta.toml")).unwrap(),
            "beta"
        );
        assert!(
            !s.new_root().join("workspaces").exists(),
            "the stranded un-suffixed dir must be folded in and removed"
        );
    }

    /// A box that already ran the broken migration once (a pre-fix build)
    /// has the stranding baked in from a PRIOR boot — not created fresh by
    /// THIS boot's `migrate_legacy_windows_config_dir` (that step is a
    /// no-op here: `app_config_dir()`'s backend children are already
    /// non-empty). Exercises `migrate_legacy_state_dirs` folding a
    /// pre-existing stranded dir on its own, with no windows-config-dir
    /// migration involved at all.
    #[test]
    #[cfg(windows)]
    fn fold_adopts_a_dir_stranded_by_an_earlier_broken_boot() {
        let _guard = env_guarded();
        std::env::set_var("SOT_SELF_HOST", "host");
        let s = MigrationScratch::new("fold-stranded");
        s.apply_env();
        let new_root = s.new_root();
        std::fs::create_dir_all(new_root.join("workspaces-host")).unwrap();
        std::fs::write(new_root.join("workspaces-host").join("gamma.toml"), "gamma").unwrap();
        std::fs::create_dir_all(new_root.join("workspaces")).unwrap();
        std::fs::write(new_root.join("workspaces").join("delta.toml"), "delta").unwrap();

        migrate_legacy_state_dirs();

        let dst = new_root.join("workspaces-host");
        assert_eq!(
            std::fs::read_to_string(dst.join("gamma.toml")).unwrap(),
            "gamma"
        );
        assert_eq!(
            std::fs::read_to_string(dst.join("delta.toml")).unwrap(),
            "delta"
        );
        assert!(!new_root.join("workspaces").exists());
    }

    /// Collision: the destination already has a toml with the same name.
    /// The destination row wins outright; the source file is left exactly
    /// where it was (not deleted, not overwritten) and the now-not-actually
    /// -empty legacy dir is left too, not removed — same rule
    /// `migrate_legacy_windows_config_dir`'s own secondary-candidate merge
    /// uses (`merge_secondary_legacy_windows_children`).
    #[test]
    #[cfg(windows)]
    fn fold_keeps_the_destination_row_on_a_name_collision() {
        let _guard = env_guarded();
        std::env::set_var("SOT_SELF_HOST", "host");
        let s = MigrationScratch::new("fold-collision");
        s.apply_env();
        let new_root = s.new_root();
        std::fs::create_dir_all(new_root.join("workspaces-host")).unwrap();
        std::fs::write(
            new_root.join("workspaces-host").join("alpha.toml"),
            "canonical",
        )
        .unwrap();
        std::fs::create_dir_all(new_root.join("workspaces")).unwrap();
        std::fs::write(new_root.join("workspaces").join("alpha.toml"), "stray").unwrap();

        migrate_legacy_state_dirs();

        assert_eq!(
            std::fs::read_to_string(new_root.join("workspaces-host").join("alpha.toml")).unwrap(),
            "canonical"
        );
        assert_eq!(
            std::fs::read_to_string(new_root.join("workspaces").join("alpha.toml")).unwrap(),
            "stray"
        );
        assert!(
            new_root.join("workspaces").is_dir(),
            "non-empty legacy dir must not be removed"
        );
    }
}
