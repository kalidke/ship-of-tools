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
