//! Tests of create.rs: the duplicate-root and same-slug gates.

use super::*;

mod duplicate_root_tests {
    use super::find_other_workspace_with_root;
    use crate::workspaces::{Workspace, Workspaces};
    use std::path::{Path, PathBuf};

    /// Unique on-disk dir per test (no tempfile dev-dep; pid + a counter keep
    /// parallel tests from colliding). Never cleaned up — OS temp is fine.
    fn scratch_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "sot-duproot-{}-{}-{}",
            std::process::id(),
            tag,
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).expect("create scratch dir");
        d
    }

    fn ws(label: &str, root: &Path) -> Workspace {
        Workspace::from_label(label, root.to_path_buf(), false, "none".into(), String::new(), String::new())
    }

    fn reg(rows: Vec<Workspace>) -> Workspaces {
        let r = Workspaces::new();
        for w in rows {
            r.insert(w);
        }
        r
    }

    #[test]
    fn same_root_different_slug_is_found() {
        let root = scratch_dir("hit");
        let existing = reg(vec![ws("sot", &root)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("a second identity for one root must be caught");
        assert_eq!(hit.slug, "sot");
    }

    #[test]
    fn same_slug_is_invisible_so_refresh_stays_allowed() {
        // A same-slug create is Workspaces::insert's id-preserving metadata
        // refresh; the gate must not turn that idempotent path into an error.
        let root = scratch_dir("refresh");
        let existing = reg(vec![ws("sot", &root)]);
        let canon = root.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "sot", &existing).is_none());
    }

    #[test]
    fn different_roots_pass() {
        let a = scratch_dir("a");
        let b = scratch_dir("b");
        let existing = reg(vec![ws("sot", &a)]);
        let canon = b.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_spelling_of_a_registered_root_still_collides() {
        // The incident shape with a twist: the duplicate is registered via a
        // symlink to the same directory. Canonical comparison must see through
        // it — path-string comparison would not.
        let root = scratch_dir("real");
        let link = std::env::temp_dir().join(format!("sot-duproot-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&root, &link).expect("create symlink");
        let existing = reg(vec![ws("sot", &link)]);
        let canon = root.canonicalize().unwrap();
        let hit = find_other_workspace_with_root(&canon, "ship-of-tools", &existing)
            .expect("symlinked duplicate must be caught");
        assert_eq!(hit.slug, "sot");
    }

    /// ADR 0042 amendment: the inert default anchor (the default row with no
    /// agent, root = the home dir, any runtime) is not a session, so a session
    /// created at that root passes the gate — while a default row that
    /// carries an agent is still refused.
    #[test]
    fn inert_default_anchor_does_not_block_a_session_at_its_root() {
        let root = scratch_dir("anchor");
        let canon = root.canonicalize().unwrap();
        let existing = Workspaces::new();
        let mut anchor = ws("local", &root);
        anchor.runtime = "capsule".to_string();
        let anchor = existing.insert(anchor);
        existing.set_default(&anchor.workspace_id);
        assert!(
            find_other_workspace_with_root(&canon, "home-session", &existing).is_none(),
            "the inert anchor must not claim its root against a real session"
        );
        // Control: the default row WITH an agent is a real session and is
        // still caught (same-slug insert keeps the id, so it stays default).
        let mut sot = ws("local", &root);
        sot.agent = std::sync::Mutex::new("claude".to_string());
        existing.insert(sot);
        assert!(find_other_workspace_with_root(&canon, "home-session", &existing).is_some());
    }

    #[test]
    fn registered_root_that_no_longer_resolves_is_skipped_not_fatal() {
        // A workspace whose root was deleted is Phase 2's (reap) problem; the
        // gate must neither match it nor error on it.
        let gone = std::env::temp_dir().join(format!("sot-duproot-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        let live = scratch_dir("live");
        let existing = reg(vec![ws("dead", &gone)]);
        let canon = live.canonicalize().unwrap();
        assert!(find_other_workspace_with_root(&canon, "other", &existing).is_none());
    }
}

mod label_in_use_tests {
    use super::same_slug_row_in_use;
    use crate::workspaces::{Observation, Phase, SupervisorIdentity, Workspace, Workspaces};

    fn row(label: &str, runtime: &str, observed: Option<Phase>) -> Workspace {
        let mut w = Workspace::from_label(
            label,
            std::env::temp_dir(),
            false,
            "none".into(),
            String::new(),
            String::new(),
        );
        w.runtime = runtime.to_string();
        if let Some(phase) = observed {
            assert!(w.apply_phase_observation(Observation::Phase {
                phase,
                supervisor: SupervisorIdentity { pid: 1, created: 1 },
                voyage: Some(uuid::Uuid::from_u128(1)),
            }));
            assert_eq!(w.phase(), phase);
        }
        w
    }

    fn reg(w: Workspace) -> Workspaces {
        let r = Workspaces::new();
        r.insert(w);
        r
    }

    #[test]
    fn stopped_capsule_row_is_not_in_use_so_the_refresh_stays_allowed() {
        let r = reg(row("sot", "capsule", None));
        assert!(same_slug_row_in_use("sot", &r).is_none());
    }

    #[test]
    fn every_observed_phase_is_in_use() {
        for phase in [
            Phase::Starting,
            Phase::Ready,
            Phase::Ending,
            Phase::EndedNoRespawn,
            Phase::Terminal,
        ] {
            let r = reg(row("sot", "capsule", Some(phase)));
            let hit = same_slug_row_in_use("sot", &r)
                .unwrap_or_else(|| panic!("phase {phase:?} must be in use"));
            assert_eq!(hit.slug, "sot");
        }
    }

    #[test]
    fn a_non_capsule_row_is_in_use_even_when_stopped() {
        let r = reg(row("sot", "tmux", None));
        assert!(same_slug_row_in_use("sot", &r).is_some());
    }

    #[test]
    fn a_different_slug_is_not_this_gates_question() {
        let r = reg(row("sot", "capsule", Some(Phase::Ready)));
        assert!(same_slug_row_in_use("other", &r).is_none());
    }
}
