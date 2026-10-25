use super::*;
use crate::net::transport::WorkspaceInfo;
use crate::ui::{agent_tone_from, ws_info, AgentTone};

fn row(slug: &str, state: &str, at: &str) -> WorkspaceInfo {
    let mut r = ws_info(slug, &format!("attach-{slug}"));
    r.agent_state = state.into();
    r.agent_status_at = at.into();
    r
}

fn replace(d: &mut RowDirectory, host: &str, rows: Vec<WorkspaceInfo>) {
    let update = d.prepare_replace(host.into(), rows);
    d.commit_replace(update);
}

fn refresh(p: &mut RowPresentation, d: &RowDirectory, now: Instant) -> PresentationEffects {
    p.refresh(
        d,
        &["a".into(), "b".into()],
        "a",
        None,
        HashSet::new(),
        || now,
    )
}

fn factor(p: &RowPresentation, slug: &str, now: Instant, expected: f32) {
    assert!(
        (p.flash_factor_for("a", slug, now) - expected).abs() < 1e-6,
        "{slug}: expected {expected}"
    );
}

#[test]
fn a_person_wait_is_red_and_a_machine_wait_is_purple() {
    let mut d = RowDirectory::default();
    let cases = [
        ("working", AgentTone::Working, (56, 152, 38)),
        ("waiting", AgentTone::Waiting, (149, 88, 178)),
        ("blocked", AgentTone::Blocked, (203, 60, 51)),
        ("done", AgentTone::Done, (64, 99, 216)),
    ];
    let mut rows: Vec<_> = cases
        .iter()
        .map(|(s, _, _)| row(s, s, "2026-09-08T09:00:00Z"))
        .collect();
    rows.extend([
        row("idle", "idle", ""),
        row("empty", "", ""),
        row("unknown", "unknown", ""),
    ]);
    replace(&mut d, "a", rows);
    let mut p = RowPresentation::default();
    refresh(&mut p, &d, Instant::now());
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-08T09:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for (slug, expected, rgb) in cases {
        let (state, at) = p.state("a", slug).unwrap();
        let (tone, wilted) = agent_tone_from(state, at, now).unwrap();
        assert_eq!((tone, wilted), (expected, false));
        assert_eq!(tone.rgb(), Some(rgb));
    }
    let (state, at) = p.state("a", "idle").unwrap();
    let (tone, wilted) = agent_tone_from(state, at, now).unwrap();
    assert_eq!((tone, wilted), (AgentTone::Idle, false));
    let (r, g, b) = tone.rgb().unwrap();
    assert_eq!((r, g), (g, b));
    for slug in ["empty", "unknown"] {
        let (state, at) = p.state("a", slug).unwrap();
        assert_eq!(agent_tone_from(state, at, now), None);
    }
}

#[test]
fn only_working_wilts_after_ten_minutes() {
    let stamp = "2026-09-08T09:00:00Z";
    let mut d = RowDirectory::default();
    replace(
        &mut d,
        "a",
        vec![
            row("working", "working", stamp),
            row("bad", "working", "malformed"),
            row("missing", "working", ""),
            row("idle", "idle", stamp),
            row("waiting", "waiting", stamp),
            row("blocked", "blocked", stamp),
            row("done", "done", stamp),
        ],
    );
    let mut p = RowPresentation::default();
    refresh(&mut p, &d, Instant::now());
    let boundary = chrono::DateTime::parse_from_rfc3339("2026-09-08T09:10:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    for slug in [
        "working", "bad", "missing", "idle", "waiting", "blocked", "done",
    ] {
        let (state, at) = p.state("a", slug).unwrap();
        assert!(
            !agent_tone_from(state, at, boundary).unwrap().1,
            "{slug} at ten minutes"
        );
        assert_eq!(
            agent_tone_from(state, at, boundary + chrono::Duration::milliseconds(1))
                .unwrap()
                .1,
            slug == "working",
            "{slug} beyond ten minutes"
        );
    }
}

#[test]
fn only_known_state_changes_start_or_restart_a_flash() {
    let t = Instant::now();
    let mut d = RowDirectory::default();
    let mut p = RowPresentation::default();
    for (state, stamp, ms, expected) in [
        ("idle", "1", 0, 0.0),
        ("idle", "2", 100, 0.0),
        ("working", "2", 200, 1.0),
        ("working", "3", 500, 0.5),
        ("blocked", "3", 600, 1.0),
    ] {
        replace(&mut d, "a", vec![row("r", state, stamp)]);
        let now = t + Duration::from_millis(ms);
        refresh(&mut p, &d, now);
        factor(&p, "r", now, expected);
    }
    // Empty is still a known prior wire state; first appearance alone never flashes.
    replace(&mut d, "a", vec![row("empty", "", "")]);
    refresh(&mut p, &d, t + Duration::from_secs(1));
    factor(&p, "empty", t + Duration::from_secs(1), 0.0);
    replace(&mut d, "a", vec![row("empty", "done", "")]);
    refresh(&mut p, &d, t + Duration::from_secs(2));
    factor(&p, "empty", t + Duration::from_secs(2), 1.0);
}

#[test]
fn missing_rows_keep_their_transition_memory() {
    let t = Instant::now();
    let mut d = RowDirectory::default();
    let mut p = RowPresentation::default();
    replace(&mut d, "a", vec![row("same", "idle", "")]);
    replace(&mut d, "b", vec![row("same", "working", "")]);
    refresh(&mut p, &d, t);
    replace(&mut d, "a", vec![]);
    refresh(&mut p, &d, t);
    assert_eq!(p.state("a", "same"), None);
    replace(&mut d, "a", vec![row("same", "idle", "")]);
    refresh(&mut p, &d, t);
    factor(&p, "same", t, 0.0);
    replace(&mut d, "a", vec![]);
    refresh(&mut p, &d, t);
    replace(&mut d, "a", vec![row("same", "done", "")]);
    refresh(&mut p, &d, t);
    factor(&p, "same", t, 1.0);
    assert_eq!(p.flash_factor_for("b", "same", t), 0.0);
}

#[test]
fn flash_fades_for_600ms_and_pruning_ends_animation() {
    let t = Instant::now();
    let mut d = RowDirectory::default();
    let mut p = RowPresentation::default();
    replace(&mut d, "a", vec![row("r", "idle", "")]);
    refresh(&mut p, &d, t);
    assert!(!p.has_flashes());
    replace(&mut d, "a", vec![row("r", "working", "")]);
    refresh(&mut p, &d, t);
    factor(&p, "r", t, 1.0);
    factor(&p, "r", t + Duration::from_millis(300), 0.5);
    factor(&p, "r", t + Duration::from_millis(600), 0.0);
    factor(&p, "unknown", t, 0.0);
    // Preserve the existing f32-to-Duration conversion and check its exact expiry boundary.
    let expiry = t + Duration::from_secs_f32(0.6);
    assert!(p.prune_expired_flashes(expiry - Duration::from_nanos(1)));
    assert!(p.has_flashes());
    assert!(!p.prune_expired_flashes(expiry));
    assert!(!p.has_flashes());
    factor(&p, "r", expiry, 0.0);
}

#[test]
fn destroy_reply_forgets_only_label_and_root() {
    let t = Instant::now();
    let mut d = RowDirectory::default();
    let mut p = RowPresentation::default();
    for state in ["idle", "working"] {
        let mut r = row("r", state, "stamp");
        r.is_default = true;
        r.label = "Named".into();
        replace(&mut d, "a", vec![r]);
        replace(&mut d, "b", vec![row("r", "blocked", "other stamp")]);
        refresh(&mut p, &d, t);
    }
    let keys = p.keys().to_vec();
    p.forget_destroyed_metadata(&("a".into(), "r".into()));
    assert_eq!(p.label("a", "r"), None);
    assert_eq!(p.project_root("a", "r"), None);
    assert_eq!(p.state("a", "r"), Some(("working", "stamp")));
    assert_eq!(p.keys(), keys.as_slice());
    assert_eq!(p.default_slug(), Some("r"));
    factor(&p, "r", t, 1.0);
    assert_eq!(p.label("b", "r"), Some("r"));
    assert_eq!(p.project_root("b", "r"), Some("/projects/r"));
    assert_eq!(p.state("b", "r"), Some(("blocked", "other stamp")));
    assert!(!p.prune_expired_flashes(t + Duration::from_secs(1)));
    let mut r = row("r", "blocked", "stamp");
    r.is_default = true;
    replace(&mut d, "a", vec![r]);
    refresh(&mut p, &d, t + Duration::from_secs(2));
    factor(&p, "r", t + Duration::from_secs(2), 1.0);
}

#[test]
fn refresh_exports_only_nonempty_repl_catchup() {
    let mut d = RowDirectory::default();
    let mut ready = row("ready", "", "");
    ready.repl_state = "ready".into();
    let mut anchor = row("anchor", "", "");
    anchor.is_default = true;
    anchor.agent = "none".into();
    anchor.repl_state = "starting".into();
    replace(&mut d, "a", vec![ready, row("old-daemon", "", ""), anchor]);
    let mut p = RowPresentation::default();
    let effects = refresh(&mut p, &d, Instant::now());
    assert_eq!(
        effects.repl_catchup,
        HashMap::from([
            (("a".into(), "ready".into()), "ready".into()),
            (("a".into(), "anchor".into()), "starting".into())
        ])
    );
    replace(
        &mut d,
        "a",
        vec![row("ready", "", ""), row("old-daemon", "", "")],
    );
    let empty = refresh(&mut p, &d, Instant::now());
    assert!(empty.repl_catchup.is_empty());
}

#[test]
fn capture_seed_preserves_selection_states_and_flash_setup() {
    let t = Instant::now();
    let stamp = "2026-09-08T09:00:00Z";
    let sessions: Vec<String> = ["one", "two", "three"].map(str::to_string).into();
    let states = vec![Some("idle".into()), Some("working".into()), None];
    let mut p = RowPresentation::default();
    let selection = p.seed_capture(&sessions, &states, &[], stamp, || t);
    assert_eq!(selection, Some(("demo".into(), "two".into())));
    let keys: Vec<_> = sessions
        .iter()
        .map(|s| ("demo".into(), s.clone()))
        .collect();
    assert_eq!(p.keys(), keys.as_slice());
    for slug in &sessions {
        assert_eq!(p.label("demo", slug), Some(slug.as_str()));
        assert_eq!(p.project_root("demo", slug), None);
    }
    assert_eq!(p.default_slug(), None);
    assert_eq!(p.state("demo", "three"), None);
    let now = chrono::DateTime::parse_from_rfc3339(stamp)
        .unwrap()
        .with_timezone(&chrono::Utc);
    let (state, at) = p.state("demo", "two").unwrap();
    assert_eq!(
        agent_tone_from(state, at, now),
        Some((AgentTone::Working, false))
    );
    let mut d = RowDirectory::default();
    replace(
        &mut d,
        "demo",
        vec![
            row("one", "blocked", stamp),
            row("two", "working", stamp),
            row("three", "done", stamp),
        ],
    );
    p.refresh(
        &d,
        &["demo".into()],
        "demo",
        Some("two"),
        HashSet::new(),
        || t,
    );
    assert_eq!(p.flash_factor_for("demo", "one", t), 1.0);
    assert_eq!(p.flash_factor_for("demo", "two", t), 0.0);
    assert_eq!(p.flash_factor_for("demo", "three", t), 0.0);
    let mut flashed = RowPresentation::default();
    flashed.seed_capture(
        &sessions,
        &states,
        &["one".into(), "two".into()],
        stamp,
        || t,
    );
    for slug in ["one", "two"] {
        assert_eq!(flashed.flash_factor_for("demo", slug, t), 1.0);
    }
    // Explicit flashes seed a different prior state for both idle and working rows.
    replace(
        &mut d,
        "demo",
        vec![row("one", "idle", stamp), row("two", "working", stamp)],
    );
    let later = t + Duration::from_millis(300);
    flashed.refresh(&d, &["demo".into()], "demo", None, HashSet::new(), || later);
    for slug in ["one", "two"] {
        assert_eq!(flashed.flash_factor_for("demo", slug, later), 1.0);
    }
    let mut standalone = RowPresentation::default();
    assert_eq!(
        standalone.seed_capture(&[], &[], &["alone".into()], stamp, || t),
        None
    );
    assert!(standalone.keys().is_empty());
    assert!(standalone.has_flashes());
    assert_eq!(standalone.flash_factor_for("demo", "alone", t), 1.0);
}

#[test]
fn refresh_samples_time_only_for_each_known_state_change() {
    let t = Instant::now();
    let mut d = RowDirectory::default();
    let mut p = RowPresentation::default();
    let hosts = ["a".into(), "b".into()];
    replace(
        &mut d,
        "a",
        vec![row("r", "idle", ""), row("unchanged", "working", "")],
    );
    replace(&mut d, "b", vec![row("r", "idle", "")]);
    p.refresh(&d, &hosts, "a", None, HashSet::new(), || {
        panic!("first appearances must not sample flash time")
    });
    p.refresh(&d, &hosts, "a", None, HashSet::new(), || {
        panic!("unchanged states must not sample flash time")
    });
    replace(
        &mut d,
        "a",
        vec![
            row("r", "working", ""),
            row("unchanged", "working", ""),
            row("new", "done", ""),
        ],
    );
    replace(&mut d, "b", vec![row("r", "blocked", "")]);
    let mut calls = 0;
    p.refresh(&d, &hosts, "a", None, HashSet::new(), || {
        let sampled = t + Duration::from_millis(calls * 300);
        calls += 1;
        sampled
    });
    assert_eq!(calls, 2);
    let at = t + Duration::from_millis(300);
    // Transition iteration uses a map; either row may receive the first sample.
    let mut factors = [
        p.flash_factor_for("a", "r", at),
        p.flash_factor_for("b", "r", at),
    ];
    factors.sort_by(|a, b| a.partial_cmp(b).unwrap());
    assert!((factors[0] - 0.5).abs() < 1e-6);
    assert_eq!(factors[1], 1.0);
    assert_eq!(p.flash_factor_for("a", "unchanged", at), 0.0);
    assert_eq!(p.flash_factor_for("a", "new", at), 0.0);
}

#[test]
fn capture_samples_time_only_for_each_explicit_flash() {
    let t = Instant::now();
    let mut p = RowPresentation::default();
    let sessions = ["one".into(), "two".into()];
    let states = [Some("idle".into()), Some("working".into())];
    let _ = p.seed_capture(&sessions, &states, &[], "stamp", || {
        panic!("seeding rows without explicit flashes must not sample flash time")
    });
    let mut calls = 0;
    let _ = p.seed_capture(&sessions, &states, &sessions, "stamp", || {
        let sampled = t + Duration::from_millis(calls * 300);
        calls += 1;
        sampled
    });
    assert_eq!(calls, 2);
    let at = t + Duration::from_millis(300);
    assert!((p.flash_factor_for("demo", "one", at) - 0.5).abs() < 1e-6);
    assert_eq!(p.flash_factor_for("demo", "two", at), 1.0);
}
