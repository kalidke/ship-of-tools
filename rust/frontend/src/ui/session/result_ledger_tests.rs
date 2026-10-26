use super::*;
use crate::net::transport::WorkspaceInfo;
use crate::ui::ws_info;

fn row(slug: &str, id: &str) -> WorkspaceInfo {
    let mut row = ws_info(slug, "stored-target");
    row.workspace_id = id.into();
    row
}

fn list(dir: &mut RowDirectory, host: &HostKey, rows: Vec<WorkspaceInfo>) {
    let update = dir.prepare_replace(host.clone(), rows);
    dir.commit_replace(update);
}

fn setup() -> (RowDirectory, ResultLedger, HostKey, HostKey) {
    let (a, b) = ("alpha".to_string(), "beta".to_string());
    let mut dir = RowDirectory::default();
    for host in [&a, &b] {
        list(
            &mut dir,
            host,
            vec![row("project", "shared-id"), row("other", "other-id")],
        );
    }
    (dir, ResultLedger::default(), a, b)
}

fn record(
    ledger: &mut ResultLedger,
    dir: &RowDirectory,
    host: &HostKey,
    spelling: &str,
    path: &str,
) {
    ledger.record(dir, host, spelling, path.into()).unwrap();
    ledger.refresh_badges(dir);
}

fn start(ledger: &mut ResultLedger, target: &ResolvedWorkspace, generation: u64) -> AttemptStart {
    ledger.begin_attempt(target, || generation + 1).unwrap().unwrap()
}

fn present(
    ledger: &mut ResultLedger,
    target: Option<&ResolvedWorkspace>,
    cursor: Option<&str>,
    shown: Option<&str>,
) -> bool {
    let effects = ledger.acknowledge_presented(target, cursor, shown);
    assert!(!effects.clear_reveal);
    effects.refresh_badges
}

#[test]
fn owed_results_use_canonical_rows_and_listed_badges() {
    let (dir, mut ledger, a, b) = setup();
    record(&mut ledger, &dir, &a, "project", "first.rs");
    record(&mut ledger, &dir, &b, "shared-id", "beta.rs");
    record(&mut ledger, &dir, &a, "other-id", "other.rs");
    record(&mut ledger, &dir, &a, "shared-id", "newest.rs");
    for (host, spelling, path) in [
        (&a, "project", "newest.rs"),
        (&b, "project", "beta.rs"),
        (&a, "other", "other.rs"),
    ] {
        let target = dir.resolve(host, spelling).unwrap();
        assert!(ledger.contains_identity(target.identity()));
        assert_eq!(ledger.path(target.identity()), Some(path));
        assert!(ledger.has_badge(target.row_key()));
    }
    assert_eq!(ledger.badged_keys(&dir).len(), 3);
    assert!(!ledger.has_badge(&(a.clone(), "shared-id".into())));
    let before = ledger.clone();
    assert_eq!(
        ledger.record(&dir, &a, "missing", "bad.rs".into()),
        Err("unknown workspace".into())
    );
    assert_eq!(ledger, before);
}

#[test]
fn replacements_and_retries_have_distinct_serials() {
    let (dir, mut ledger, a, _) = setup();
    let target = dir.resolve(&a, "project").unwrap();
    assert!(ledger.begin_attempt(&target, || 41).unwrap().is_none());
    record(&mut ledger, &dir, &a, "project", "same.rs");
    let first = start(&mut ledger, &target, 40);
    assert_eq!(
        (
            first.id.result_serial,
            first.id.attempt_serial,
            first.preview_gen
        ),
        (1, 1, 41)
    );
    let retry = start(&mut ledger, &target, first.preview_gen);
    assert_eq!(retry.id.result_serial, first.id.result_serial);
    assert!(retry.id.attempt_serial > first.id.attempt_serial);
    assert_eq!(retry.preview_gen, 42);
    ledger.abandon_attempt(&dir, &a, &retry.id);
    assert_eq!(ledger.path(target.identity()), Some("same.rs"));
    assert!(ledger.has_badge(target.row_key()));
    let after_abandon = start(&mut ledger, &target, retry.preview_gen);
    assert!(after_abandon.id.attempt_serial > retry.id.attempt_serial);
    record(&mut ledger, &dir, &a, "shared-id", "same.rs");
    let replacement = start(&mut ledger, &target, after_abandon.preview_gen);
    assert!(replacement.id.result_serial > after_abandon.id.result_serial);
    assert!(replacement.id.attempt_serial > after_abandon.id.attempt_serial);
    ledger.row_outcome(target.identity(), ResultRowOutcome::Removed, &a);
    record(&mut ledger, &dir, &a, "project", "same.rs");
    let after_removal = start(&mut ledger, &target, replacement.preview_gen);
    assert!(after_removal.id.result_serial > replacement.id.result_serial);
    assert!(after_removal.id.attempt_serial > replacement.id.attempt_serial);
}

#[test]
fn only_the_current_listed_attempt_is_admitted() {
    let (mut dir, mut ledger, a, b) = setup();
    let target = dir.resolve(&a, "project").unwrap();
    record(&mut ledger, &dir, &a, "project", "one.rs");
    let first = start(&mut ledger, &target, 0);
    ledger.arm_reveal(first.id.clone());
    assert!(ledger.admits_attempt(&dir, &a, &a, &first.id));
    assert!(!ledger.admits_attempt(&dir, &a, &b, &first.id));
    assert!(!ledger.admits_attempt(&dir, &b, &a, &first.id));
    let mut slug = first.id.clone();
    slug.workspace_id = "project".into();
    let mut result = first.id.clone();
    result.result_serial += 1;
    let mut attempt = first.id.clone();
    attempt.attempt_serial += 1;
    // Arm each supplied token so marker equality cannot mask a missing identity/serial check.
    for wrong in [slug, result, attempt] {
        ledger.arm_reveal(wrong.clone());
        assert!(!ledger.admits_attempt(&dir, &a, &a, &wrong));
    }
    ledger.arm_reveal(first.id.clone());
    assert!(!ledger.admits_attempt(&dir, &a, &a, &result_token(&first.id)));
    ledger.release_reveal();
    assert!(!ledger.admits_attempt(&dir, &a, &a, &first.id));
    ledger.arm_reveal(first.id.clone());
    record(&mut ledger, &dir, &a, "project", "one.rs");
    assert!(!ledger.admits_attempt(&dir, &a, &a, &first.id));
    let second = start(&mut ledger, &target, first.preview_gen);
    ledger.arm_reveal(second.id.clone());
    let retry = start(&mut ledger, &target, second.preview_gen);
    assert!(!ledger.admits_attempt(&dir, &a, &a, &second.id));
    assert!(!ledger.admits_attempt(&dir, &a, &a, &retry.id));
    ledger.arm_reveal(retry.id.clone());
    assert!(ledger.admits_attempt(&dir, &a, &a, &retry.id));
    // Commit without reconciliation here to isolate admission's current-listing check.
    list(&mut dir, &a, vec![]);
    assert!(!ledger.admits_attempt(&dir, &a, &a, &retry.id));
}

fn result_token(id: &ResultAttemptId) -> ResultAttemptId {
    let mut other = id.clone();
    other.result_serial += 1;
    other
}

#[test]
fn acknowledgment_requires_row_cursor_preview_and_presentation() {
    let (dir, mut ledger, a, b) = setup();
    let target = dir.resolve(&a, "project").unwrap();
    let other = dir.resolve(&b, "project").unwrap();
    let mut generation = 10;
    for cursor_first in [true, false] {
        record(&mut ledger, &dir, &a, "project", "same.rs");
        let attempt = start(&mut ledger, &target, generation);
        generation = attempt.preview_gen;
        ledger.arm_reveal(attempt.id.clone());
        let node = Some(attempt.node_id.as_str());
        assert!(!present(&mut ledger, Some(&target), node, node));
        ledger.cursor_landed(&dir, &b, &attempt.node_id);
        ledger.cursor_landed(&dir, &a, "files:wrong.rs");
        ledger.preview_installed(Some(&other), attempt.preview_gen, node);
        ledger.preview_installed(Some(&target), attempt.preview_gen + 1, node);
        ledger.preview_installed(Some(&target), attempt.preview_gen, Some("files:wrong.rs"));
        assert!(!present(&mut ledger, Some(&target), node, node));
        if cursor_first {
            ledger.cursor_landed(&dir, &a, &attempt.node_id);
        } else {
            ledger.preview_installed(Some(&target), attempt.preview_gen, node);
        }
        assert!(!present(&mut ledger, Some(&target), node, node));
        if cursor_first {
            ledger.preview_installed(Some(&target), attempt.preview_gen, node);
        } else {
            ledger.cursor_landed(&dir, &a, &attempt.node_id);
        }
        assert!(
            ledger.reveal_attempt().is_none(),
            "cursor landing releases the marker, not the certificates"
        );
        assert_eq!(ledger.path(target.identity()), Some("same.rs"));
        assert!(
            ledger.has_badge(target.row_key()),
            "certification is not presentation"
        );
        for (row, cursor, shown) in [
            (None, node, node),
            (Some(&other), node, node),
            (Some(&target), None, node),
            (Some(&target), node, None),
            (Some(&target), Some("files:wrong.rs"), node),
            (Some(&target), node, Some("files:wrong.rs")),
        ] {
            assert!(!present(&mut ledger, row, cursor, shown));
        }
        assert!(present(&mut ledger, Some(&target), node, node));
        ledger.refresh_badges(&dir);
        assert!(!ledger.contains_identity(target.identity()));
        assert!(!ledger.has_badge(target.row_key()));
        assert!(!present(&mut ledger, Some(&target), node, node));
    }
    record(&mut ledger, &dir, &a, "project", "same.rs");
    let old = start(&mut ledger, &target, 50);
    ledger.arm_reveal(old.id.clone());
    ledger.cursor_landed(&dir, &a, &old.node_id);
    ledger.preview_installed(Some(&target), old.preview_gen, Some(&old.node_id));
    generation = old.preview_gen;
    // Neither replacement nor retry inherits the old same-node certificates.
    for replace in [true, false] {
        if replace {
            record(&mut ledger, &dir, &a, "project", "same.rs");
            assert!(
                !present(
                    &mut ledger,
                    Some(&target),
                    Some(&old.node_id),
                    Some(&old.node_id)
                ),
                "replacement inherited presentation certificates from the old result"
            );
        }
        let current = start(&mut ledger, &target, generation);
        generation = current.preview_gen;
        ledger.arm_reveal(current.id.clone());
        ledger.cursor_landed(&dir, &a, &current.node_id);
        ledger.preview_installed(Some(&target), old.preview_gen, Some(&old.node_id));
        assert!(!present(
            &mut ledger,
            Some(&target),
            Some(&current.node_id),
            Some(&current.node_id)
        ));
        ledger.preview_installed(Some(&target), current.preview_gen, Some(&current.node_id));
        assert!(ledger.contains_identity(target.identity()));
    }
}

#[test]
fn retirement_ends_only_its_canonical_result_and_attempt() {
    let (mut dir, mut ledger, a, b) = setup();
    let target = dir.resolve(&a, "project").unwrap();
    let other_host = dir.resolve(&b, "project").unwrap();
    record(&mut ledger, &dir, &a, "project", "old.rs");
    record(&mut ledger, &dir, &b, "project", "beta.rs");
    let foreign = start(&mut ledger, &other_host, 0);
    let old = start(&mut ledger, &target, 0);
    ledger.arm_reveal(old.id.clone());
    let update = dir.prepare_replace(
        a.clone(),
        vec![row("project", "successor"), row("other", "other-id")],
    );
    assert_eq!(
        ledger.reconcile_retired(update.retired(), &a),
        ResultEffects {
            refresh_badges: true,
            clear_reveal: true
        }
    );
    assert!(
        dir.resolve(&a, "shared-id").is_ok(),
        "reconcile precedes commitment"
    );
    assert!(!ledger.contains_identity(target.identity()));
    assert!(!ledger.admits_attempt(&dir, &a, &a, &old.id));
    ledger.arm_reveal(foreign.id.clone());
    assert!(ledger.admits_attempt(&dir, &b, &b, &foreign.id));
    dir.commit_replace(update);
    record(&mut ledger, &dir, &a, "project", "new.rs");
    let successor = dir.resolve(&a, "project").unwrap();
    let new = start(&mut ledger, &successor, old.preview_gen);
    ledger.arm_reveal(new.id.clone());
    let late = ledger.row_outcome(target.identity(), ResultRowOutcome::Removed, &a);
    assert!(!late.clear_reveal);
    assert!(ledger.admits_attempt(&dir, &a, &a, &new.id));
    assert_eq!(ledger.path(successor.identity()), Some("new.rs"));
    assert_eq!(ledger.path(other_host.identity()), Some("beta.rs"));
    let removed = ledger.row_outcome(successor.identity(), ResultRowOutcome::Removed, &a);
    assert!(removed.clear_reveal && removed.refresh_badges);
    assert!(!ledger.contains_identity(successor.identity()));
    assert!(!ledger.admits_attempt(&dir, &a, &a, &new.id));
    assert_eq!(ledger.path(other_host.identity()), Some("beta.rs"));
    // Plain removal also reconciles before commitment; a foreign active host gets no reveal cleanup.
    let update = dir.prepare_replace(b.clone(), vec![]);
    assert_eq!(
        ledger.reconcile_retired(update.retired(), &a),
        ResultEffects {
            refresh_badges: true,
            clear_reveal: false
        }
    );
    assert!(!ledger.contains_identity(other_host.identity()));
    dir.commit_replace(update);
}

#[test]
fn kept_default_preserves_its_result_and_attempt() {
    let (mut dir, mut ledger, a, _) = setup();
    let mut default = row("base", "default-id");
    default.is_default = true;
    list(&mut dir, &a, vec![default.clone()]);
    let target = dir.resolve(&a, "").unwrap();
    record(&mut ledger, &dir, &a, "default", "own.rs");
    let attempt = start(&mut ledger, &target, 0);
    ledger.arm_reveal(attempt.id.clone());
    let before = ledger.clone();
    assert_eq!(
        ledger.row_outcome(target.identity(), ResultRowOutcome::Kept, &a),
        ResultEffects::default()
    );
    let update = dir.prepare_replace(a.clone(), vec![default]);
    assert_eq!(
        ledger.reconcile_retired(update.retired(), &a),
        ResultEffects::default()
    );
    dir.commit_replace(update);
    ledger.refresh_badges(&dir);
    assert_eq!(ledger, before);
    assert_eq!(ledger.path(target.identity()), Some("own.rs"));
    assert!(ledger.admits_attempt(&dir, &a, &a, &attempt.id));
}

#[test]
fn reconciliation_tests_pending_canonical_identity_membership() {
    let (mut dir, mut ledger, a, b) = setup();
    let mut rows = vec![row("collision", "owed-id"), row("owed-id", "unowed-id")];
    rows.extend((0..30).map(|i| row(&format!("slug-{i}"), &format!("id-{i}"))));
    list(&mut dir, &a, rows);
    list(&mut dir, &b, vec![row("collision", "unowed-id")]);
    record(&mut ledger, &dir, &a, "collision", "alpha.rs");
    record(&mut ledger, &dir, &a, "slug-3", "three.rs");
    record(&mut ledger, &dir, &b, "collision", "beta.rs");
    let update = dir.prepare_replace(a.clone(), vec![]);
    for identity in update.retired() {
        let pending = ["owed-id", "id-3"].contains(&identity.workspace_id.as_str());
        assert_eq!(ledger.contains_identity(identity), pending);
        let effects = ledger.reconcile_retired(std::slice::from_ref(identity), &a);
        assert_eq!(
            effects.refresh_badges, pending,
            "unowed canonical retirement must have no effects: {identity:?}"
        );
        assert!(!effects.clear_reveal);
        assert!(!ledger.contains_identity(identity));
    }
    let foreign = dir.resolve(&b, "collision").unwrap();
    assert_eq!(ledger.path(foreign.identity()), Some("beta.rs"));
    assert_eq!(
        ledger.reconcile_retired(update.retired(), &a),
        ResultEffects::default()
    );
    dir.commit_replace(update);
    ledger.refresh_badges(&dir);
    assert_eq!(
        ledger.badged_keys(&dir),
        HashSet::from([foreign.row_key().clone()])
    );
}

#[test]
fn releasing_reveal_keeps_the_result_owed() {
    let (dir, mut ledger, a, _) = setup();
    let target = dir.resolve(&a, "project").unwrap();
    record(&mut ledger, &dir, &a, "project", "owed.rs");
    let first = start(&mut ledger, &target, 0);
    ledger.arm_reveal(first.id.clone());
    ledger.release_reveal();
    assert!(!ledger.admits_attempt(&dir, &a, &a, &first.id));
    assert_eq!(ledger.path(target.identity()), Some("owed.rs"));
    assert!(ledger.has_badge(target.row_key()));
    let retry = start(&mut ledger, &target, first.preview_gen);
    assert_ne!(retry.id, first.id);
    ledger.arm_reveal(retry.id.clone());
    // Abandonment of an obsolete issuance must not erase the current attempt.
    ledger.abandon_attempt(&dir, &a, &first.id);
    assert!(ledger.reveal_attempt().is_none());
    ledger.arm_reveal(retry.id.clone());
    assert!(ledger.admits_attempt(&dir, &a, &a, &retry.id));
    ledger.abandon_attempt(&dir, &a, &retry.id);
    assert!(ledger.reveal_attempt().is_none());
    ledger.arm_reveal(retry.id.clone());
    assert!(!ledger.admits_attempt(&dir, &a, &a, &retry.id));
    assert_eq!(ledger.path(target.identity()), Some("owed.rs"));
    assert!(ledger.has_badge(target.row_key()));
    ledger.release_reveal();
    let next = start(&mut ledger, &target, retry.preview_gen);
    assert_ne!(next.id, retry.id);
}

#[test]
fn badge_projection_tracks_a_renamed_canonical_row() {
    let (mut dir, mut ledger, a, _) = setup();
    let target = dir.resolve(&a, "project").unwrap();
    record(&mut ledger, &dir, &a, "project", "owed.rs");
    let update = dir.prepare_replace(
        a.clone(),
        vec![row("renamed", "shared-id"), row("other", "other-id")],
    );
    assert!(update.retired().is_empty());
    assert_eq!(
        ledger.reconcile_retired(update.retired(), &a),
        ResultEffects::default()
    );
    dir.commit_replace(update);
    ledger.refresh_badges(&dir);
    let renamed = dir.resolve(&a, "renamed").unwrap();
    assert_eq!(target.identity(), renamed.identity());
    assert_eq!(ledger.path(renamed.identity()), Some("owed.rs"));
    assert!(!ledger.has_badge(target.row_key()));
    assert!(ledger.has_badge(renamed.row_key()));
    assert_eq!(
        ledger.badged_keys(&dir),
        HashSet::from([renamed.row_key().clone()])
    );
}

#[test]
fn result_attempt_allocates_exactly_once_only_after_admission() {
    use crate::ui::preview::requests::PreviewRequests;
    use std::cell::Cell;
    let (dir, mut ledger, host, _) = setup();
    let target = dir.resolve(&host, "project").unwrap();
    let mut requests = PreviewRequests::default();
    let calls = Cell::new(0u64);
    assert!(ledger.begin_attempt(&target, || {
        calls.set(calls.get() + 1);
        requests.issue_preview()
    }).unwrap().is_none());
    assert_eq!((calls.get(), requests.preview_generation()), (0, 0));
    record(&mut ledger, &dir, &host, "project", "owed.rs");
    for expected in [1, 2] {
        let attempt = ledger.begin_attempt(&target, || {
            calls.set(calls.get() + 1);
            requests.issue_preview()
        }).unwrap().unwrap();
        assert_eq!((calls.get(), attempt.preview_gen, requests.preview_generation()), (expected, expected, expected));
        assert!(ledger.has_badge(target.row_key()));
        assert_eq!(ledger.path(target.identity()), Some("owed.rs"));
        ledger.abandon_attempt(&dir, &host, &attempt.id);
    }
    // Seed only the ledger's exhausted serial inside its own test module; all request state
    // is constructed and changed through public operations, with no clock setter.
    let mut exhausted = ResultLedger { attempt_serial: u64::MAX, ..ResultLedger::default() };
    for owed in [false, true] {
        if owed { record(&mut exhausted, &dir, &host, "project", "owed.rs"); }
        let result = exhausted.begin_attempt(&target, || {
            calls.set(calls.get() + 1);
            requests.issue_preview()
        });
        assert!(matches!(result, Err("result attempt serial space exhausted")));
        assert_eq!((calls.get(), requests.preview_generation()), (2, 2));
    }
}
