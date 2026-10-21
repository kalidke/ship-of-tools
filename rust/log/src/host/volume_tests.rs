use super::*;

// -----------------------------------------------------------------
// ADR 0043 decision 23: `preflight_volume` on Linux and macOS
// -----------------------------------------------------------------

#[cfg(unix)]
fn no_preflight_residue(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .all(|e| !e.file_name().to_string_lossy().starts_with(".sot-preflight-"))
}

#[test]
#[cfg(target_os = "linux")]
fn remote_fs_name_matches_the_deny_list_constants_only() {
    for &(magic, name) in REMOTE_FS_TYPES {
        assert_eq!(remote_fs_name(magic), Some(name));
    }
    // A magic not on the list is not refused BY THIS FUNCTION — the
    // rename/fsync probes are what judges an unlisted type.
    assert_eq!(remote_fs_name(0x0102_3456), None);
    // tmpfs is deliberately not on the deny list (developer `/tmp`).
    const TMPFS_MAGIC: i64 = 0x0102_1994;
    assert_eq!(remote_fs_name(TMPFS_MAGIC), None);
}

#[test]
#[cfg(unix)]
fn preflight_volume_passes_on_an_ordinary_tempdir_and_leaves_no_residue() {
    let dir = tempfile::tempdir().unwrap();
    preflight_volume(dir.path()).unwrap();
    assert!(no_preflight_residue(dir.path()), "preflight must remove its own temp entries");
}

#[test]
#[cfg(unix)]
fn preflight_volume_passes_on_tmpfs() {
    // tmpfs is ALLOWED here (decision 23: developer `/tmp` is often
    // tmpfs, and every suite must keep running there) — this is the
    // daemon's own, separate volatile-type refusal to make
    // (`rows::spawn::state_root::qualified_state_root`), never this probe's.
    // Skipped, not failed, when `/dev/shm` isn't mounted — some
    // container images omit it, and macOS never mounts tmpfs there at
    // all (no `/dev/shm`), so this always skips on macOS rather than
    // asserting anything about its filesystem.
    let base = Path::new("/dev/shm");
    if !base.is_dir() {
        eprintln!("skipping preflight_volume_passes_on_tmpfs: /dev/shm is not mounted here");
        return;
    }
    let dir = tempfile::Builder::new().prefix("sot-fsutil-tmpfs-").tempdir_in(base).unwrap();
    preflight_volume(dir.path()).unwrap();
    assert!(no_preflight_residue(dir.path()), "preflight must remove its own temp entries");
}

/// The whitebox half of "seed `b` yourself and run the probe": this
/// test reconstructs the exact `b` path [`probe_rename_noreplace_pair`]
/// will use for `kind` under a nonce of the test's own choosing (the
/// same private [`preflight_pair_paths`] the probe itself calls — no
/// separate seam exposed for this) and pre-occupies it BEFORE calling
/// the probe. `RENAME_NOREPLACE`'s own atomicity means the very first
/// (otherwise-unconditional) rename now collides too, so this also
/// proves the refusal fires even outside the probe's own
/// self-generated second-attempt collision, and that cleanup never
/// deletes an entry the probe did not itself create —
/// [`probe_rename_noreplace_pair`]'s own `own_b` tracking. (The public
/// [`preflight_volume`] mints a fresh per-call nonce precisely so no
/// outside party can predict or collide with it.)
#[cfg(unix)]
fn assert_preflight_volume_refuses_a_seeded_collision(kind: PreflightEntryKind) {
    let dir = tempfile::tempdir().unwrap();
    let nonce = "seeded-test";
    let (a, b) = preflight_pair_paths(dir.path(), kind, nonce);
    let seeded = b"seeded-before-preflight-ran";
    kind.create(&b, seeded).unwrap();

    let err = probe_rename_noreplace_pair(dir.path(), kind, nonce).unwrap_err();
    assert!(format!("{err}").contains("could not rename"), "{err}");

    // Not ours to delete: the seeded entry must survive untouched.
    assert!(kind.intact(&b, seeded), "a pre-existing, un-owned `b` must survive a refused preflight");
    // But OUR OWN dangling `a` (created before the refused rename)
    // must still be cleaned up.
    assert!(!a.exists(), "preflight must still remove its own `a` even when `b` was never its own");

    kind.remove(&b);
}

#[test]
#[cfg(unix)]
fn preflight_volume_refuses_a_seeded_collision_for_the_dir_pair() {
    assert_preflight_volume_refuses_a_seeded_collision(PreflightEntryKind::Dir);
}

#[test]
#[cfg(unix)]
fn preflight_volume_refuses_a_seeded_collision_for_the_file_pair() {
    assert_preflight_volume_refuses_a_seeded_collision(PreflightEntryKind::File);
}
