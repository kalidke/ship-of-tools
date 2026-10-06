//! Tests of the grant rule, departures and ticks, held.json, the start plan and the lease connection.

use super::*;
use sot_log::identity::challenge::PeerAuthenticated;
use std::time::Duration;

const BOOT: &str = "boot-a";
const T0: u64 = 1_000_000;

fn id(pid: u32) -> ProcessIdentity {
    ProcessIdentity { boot: BOOT.into(), pid, created: 7000 + u64::from(pid) }
}

fn req(who: &ProcessIdentity) -> FeLeaseReq {
    FeLeaseReq { boot: who.boot.clone(), pid: who.pid, created: who.created, token: None }
}

fn peer(who: &ProcessIdentity) -> PeerAuthenticated {
    PeerAuthenticated { pid: who.pid, created: who.created }
}

fn empty() -> HeldRecord {
    HeldRecord {
        v: 1,
        boot: BOOT.into(),
        holders: vec![],
        handover_until_ms: None,
        closing: false,
        not_ended: 0,
        forget: vec![],
    }
}

struct Fixture {
    leases: Leases,
    path: PathBuf,
    _dir: tempfile::TempDir,
}

fn fixture(own_boot: Option<&str>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(bounds::HELD_RECORD_FILE);
    let leases = Leases::new(own_boot.map(String::from), Some(path.clone()), None, false);
    Fixture { leases, path, _dir: dir }
}

impl Fixture {
    fn grant(&self, who: &ProcessIdentity) -> u64 {
        let (outcome, gen) = self.leases.grant(&req(who), &peer(who));
        assert_eq!(outcome, LeaseOutcome::Granted, "{who:?}");
        gen.expect("a grant carries its generation")
    }

    fn on_disk(&self) -> Option<HeldRecord> {
        read_record(&self.path).expect("the record parses")
    }

    /// The start's plan at `now`, which must be Pending, installed;
    /// its deadline.
    fn install_planned(&self, now: u64) -> u64 {
        let StartPlan::Pending { until_ms } = startup_plan(&read_record(&self.path), Ok(BOOT), now) else {
            panic!("the record plans Pending");
        };
        self.leases.install_pending(until_ms).unwrap();
        until_ms
    }
}

/// This daemon at `T0`, built on the record at `path` as the start
/// builds it.
fn start_on(path: &Path) -> Leases {
    let read = read_record(path);
    let cleanup = startup_plan(&read, Ok(BOOT), T0) == StartPlan::Cleanup;
    Leases::new(Some(BOOT.into()), Some(path.to_path_buf()), read.as_ref().ok().and_then(Option::as_ref), cleanup)
}

/// A start at `T0` over `text`, the bytes the last daemon left: the
/// record on disk first, then this daemon built on it.
fn restart_raw(text: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(bounds::HELD_RECORD_FILE);
    std::fs::write(&path, text).unwrap();
    Fixture { leases: start_on(&path), path, _dir: dir }
}

fn restart(rec: &HeldRecord) -> Fixture {
    restart_raw(&serde_json::to_string(rec).unwrap())
}

#[test]
fn not_ended_survives_restart_until_acked() {
    let f = restart(&HeldRecord { not_ended: 3, ..empty() });
    f.grant(&id(1));
    assert_eq!(f.leases.notice(), 3, "the first grant after a restart lost the record's not_ended");
    assert_eq!(f.on_disk().map(|r| r.not_ended), Some(3), "the first grant's record lost the not_ended count");
    f.leases.notice_seen(3).unwrap();
    assert_eq!(f.leases.notice(), 0, "an ack of the loaded count did not clear it");
    assert_eq!(f.on_disk().map(|r| r.not_ended), Some(0));

    let f = restart(&HeldRecord { closing: true, not_ended: 3, ..empty() });
    f.leases.finish_cleanup(2, Vec::new()).unwrap();
    assert_eq!(f.leases.notice(), 5, "a startup Cleanup's count replaced the loaded one instead of adding to it");
    assert_eq!(f.on_disk().map(|r| r.not_ended), Some(5));
}

#[test]
fn shutdown_keeps_an_unacked_loaded_count() {
    let f = restart(&HeldRecord { not_ended: 3, ..empty() });
    f.leases.begin_close();
    f.leases.finish_shutdown(2, Vec::new()).unwrap();
    assert_eq!(
        f.on_disk().map(|r| r.not_ended),
        Some(5),
        "a shutdown replaced the loaded count no window had seen instead of adding to it"
    );
}

#[test]
fn lease_during_startup_cleanup_keeps_the_record() {
    let closing = HeldRecord { closing: true, ..empty() };
    let other_boot = HeldRecord { boot: "boot-b".into(), holders: vec![id(9)], ..empty() };
    for (what, rec) in [("closing", closing), ("another boot", other_boot)] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(bounds::HELD_RECORD_FILE);
        write_or_delete(&path, &rec).unwrap();
        let leases = start_on(&path);
        let who = id(1);
        let (outcome, _) = leases.grant(&req(&who), &peer(&who));
        assert_eq!(outcome, LeaseOutcome::Granted, "{what}: a grant still answers the window");
        assert_eq!(
            read_record(&path).unwrap(),
            Some(rec.clone()),
            "{what}: a grant during the startup Cleanup rewrote the record"
        );
        leases.finish_cleanup(0, Vec::new()).unwrap();
        let after = read_record(&path).unwrap().expect("the holder is recorded once the Cleanup finishes");
        assert_eq!(after.holders, vec![who], "{what}");
        assert!(!after.closing, "{what}");
    }
}

#[tokio::test]
async fn pending_expiry_shuts_down() {
    let f = fixture(Some(BOOT));
    f.leases.install_pending(T0 + 10).unwrap();
    assert_eq!(f.leases.tick(T0 + 20), Tick::Shutdown);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), f.leases.gone()).await.is_ok(),
        "a pending start that expired with no qualifying lease did not shut down"
    );
    assert!(f.on_disk().expect("the record").closing, "the shutdown did not record closing");
}

#[test]
fn lease_claim_table() {
    use LeaseOutcome::*;
    let me = id(4242);
    let edited = |edit: fn(&mut FeLeaseReq)| {
        let mut r = req(&me);
        edit(&mut r);
        r
    };
    let cases: Vec<(&str, FeLeaseReq, PeerAuthenticated, LeaseOutcome, Option<&str>)> = vec![
        ("equal identity", req(&me), peer(&me), Granted, None),
        ("different pid", edited(|r| r.pid += 1), peer(&me), Foreign, Some("pid mismatch")),
        ("different created", edited(|r| r.created += 1), peer(&me), Foreign, Some("created mismatch")),
        ("different boot, same pid and created", edited(|r| r.boot = "boot-b".into()), peer(&me), Foreign, Some("boot mismatch")),
    ];
    let f = fixture(Some(BOOT));
    for (name, r, p, want, why) in &cases {
        let (got, gen) = f.leases.grant(r, p);
        assert_eq!(got, *want, "{name}");
        assert_eq!(gen.is_some(), *want == Granted, "{name}: a generation iff granted");
        assert_eq!(claim(Some(BOOT), r, p).err(), why.map(|why| (*want, why)), "{name}: the check named");
    }
    let a = f.grant(&me);
    let b = f.grant(&me);
    assert_ne!(a, b, "two leases from one identity are two entries");
    assert_eq!(f.on_disk().unwrap().holders, vec![me.clone()]);

    let f = fixture(None);
    assert_eq!(f.leases.grant(&req(&me), &peer(&me)).0, Undetermined, "missing daemon boot");
    assert_eq!(claim(None, &req(&me), &peer(&me)).err(), Some((Undetermined, "own boot unknown")));
    assert_eq!(f.on_disk(), None, "a refusal records nothing");

    let f = fixture(Some(BOOT));
    f.leases.begin_close();
    assert_eq!(f.leases.grant(&req(&me), &peer(&me)), (Closing, None), "closing");
}

#[cfg(unix)]
#[test]
fn grant_refused_when_record_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |f: &Fixture, m: u32| {
        std::fs::set_permissions(f.path.parent().unwrap(), std::fs::Permissions::from_mode(m)).unwrap()
    };
    let f = fixture(Some(BOOT));
    let a = f.grant(&id(1));
    mode(&f, 0o500);
    assert_eq!(f.leases.grant(&req(&id(2)), &peer(&id(2))), (LeaseOutcome::Undetermined, None), "refused, never Granted");
    mode(&f, 0o700);
    f.leases.depart(a, Some(LeaveIntent::Keep), T0);
    assert_eq!(f.on_disk(), None, "the refused grant's entry is undone");
    assert_ne!(f.grant(&id(2)), a, "a later grant has a new generation");

    let f = fixture(Some(BOOT));
    f.leases.install_pending(T0 + handover_bound_ms()).unwrap();
    mode(&f, 0o500);
    assert_eq!(f.leases.grant(&req(&id(1)), &peer(&id(1))), (LeaseOutcome::Undetermined, None));
    mode(&f, 0o700);
    assert_eq!(
        f.leases.tick(T0 + handover_bound_ms()),
        Tick::Shutdown,
        "every in-memory change is undone: no lease held, the pending start kept"
    );
}

#[tokio::test]
async fn departure_table() {
    use LeaveIntent::*;
    let intents = [None, Some(Close), Some(Keep), Some(Handover)];
    for intent in intents {
        let f = fixture(Some(BOOT));
        let a = f.grant(&id(1));
        f.grant(&id(2));
        assert_eq!(f.leases.depart(a, intent, T0), Decision::None, "not the last, {intent:?}");
        assert_eq!(f.on_disk().unwrap().holders, vec![id(2)], "{intent:?}");
        assert_eq!(f.leases.tick(T0 + 10 * handover_bound_ms()), Tick::None, "{intent:?}");
    }
    let closing = Some(HeldRecord { closing: true, ..empty() });
    let handover = Some(HeldRecord { handover_until_ms: Some(T0 + handover_bound_ms()), ..empty() });
    let wants = [
        (Decision::Shutdown, closing.clone()),
        (Decision::Shutdown, closing),
        (Decision::None, Option::None),
        (Decision::None, handover),
    ];
    for (intent, (want, rec)) in intents.into_iter().zip(wants) {
        let f = fixture(Some(BOOT));
        let a = f.grant(&id(1));
        assert_eq!(f.leases.depart(a, intent, T0), want, "the last, {intent:?}");
        let gone = tokio::time::timeout(Duration::from_millis(50), f.leases.gone()).await.is_ok();
        assert_eq!(gone, want == Decision::Shutdown, "gone fires iff shutdown, {intent:?}");
        assert_eq!(f.on_disk(), rec, "{intent:?}");
        if want == Decision::Shutdown {
            assert_eq!(f.leases.grant(&req(&id(3)), &peer(&id(3))).0, LeaseOutcome::Closing);
        }
    }
}

#[test]
fn lease_generation_fences_removal() {
    let f = fixture(Some(BOOT));
    let w = id(1);
    let first = f.grant(&w);
    let second = f.grant(&w);
    assert_eq!(f.leases.depart(first, None, T0), Decision::None, "the replaced lease is not the last");
    assert_eq!(f.on_disk().unwrap().holders, vec![w.clone()]);
    assert_eq!(f.leases.depart(first, Some(LeaveIntent::Close), T0), Decision::None, "a second end is a no-op");
    assert_eq!(f.leases.depart(9999, None, T0), Decision::None, "an unknown generation is a no-op");
    assert_eq!(f.on_disk().unwrap().holders, vec![w.clone()]);
    assert_eq!(f.leases.depart(second, Some(LeaveIntent::Keep), T0), Decision::None);
    assert_eq!(f.on_disk(), None);
    assert_eq!(f.leases.depart(second, None, T0), Decision::None, "a late end after the last never shuts down");
    assert_eq!(f.leases.depart(first, None, T0), Decision::None);
    assert_eq!(f.on_disk(), None);
    f.grant(&w);
}

#[test]
fn record_survives_other_windows_keep() {
    for intent in [LeaveIntent::Keep, LeaveIntent::Close, LeaveIntent::Handover] {
        let f = fixture(Some(BOOT));
        let a = f.grant(&id(1));
        f.grant(&id(2));
        f.leases.depart(a, Some(intent), T0);
        let rec = f.on_disk().expect("the other window's holder entry stays");
        assert_eq!(rec, HeldRecord { holders: vec![id(2)], ..empty() }, "{intent:?}");
    }
}

#[test]
fn handover_table() {
    let until = T0 + handover_bound_ms();
    let f = fixture(Some(BOOT));
    let a = f.grant(&id(1));
    assert_eq!(f.leases.depart(a, Some(LeaveIntent::Handover), T0), Decision::None);
    assert_eq!(f.on_disk().unwrap().handover_until_ms, Some(until), "persisted");
    assert_eq!(f.leases.tick(until - 1), Tick::None);
    assert_eq!(f.leases.tick(until), Tick::Shutdown, "expiry with no lease held");
    assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }));
    assert_eq!(f.leases.tick(until + 1), Tick::None, "decided once");

    let f = fixture(Some(BOOT));
    let a = f.grant(&id(1));
    f.leases.depart(a, Some(LeaveIntent::Handover), T0);
    let b = f.grant(&id(2));
    assert_eq!(f.on_disk().unwrap().handover_until_ms, None, "any grant clears it");
    assert_eq!(f.leases.tick(until + 1), Tick::None);
    assert_eq!(f.leases.depart(b, Some(LeaveIntent::Keep), T0 + 1), Decision::None);
    assert_eq!(f.on_disk(), None);
    assert_eq!(f.leases.tick(u64::MAX), Tick::None, "Keep never expires");
    f.grant(&id(3));
}

#[test]
fn pending_start_table() {
    let until = T0 + handover_bound_ms();
    let f = fixture(Some(BOOT));
    f.leases.install_pending(until).unwrap();
    assert_eq!(f.on_disk(), Some(HeldRecord { handover_until_ms: Some(until), ..empty() }));
    f.grant(&id(9));
    assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(9)], ..empty() }), "any grant completes a pending start");
    assert_eq!(f.leases.tick(until), Tick::None);

    let f = fixture(Some(BOOT));
    f.leases.install_pending(until).unwrap();
    assert_eq!(f.leases.tick(until - 1), Tick::None);
    assert_eq!(f.leases.tick(until), Tick::Shutdown, "expiry with no lease held");
    assert_eq!(f.leases.tick(until + 1), Tick::None, "decided once");
    assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }));
    assert_eq!(f.leases.grant(&req(&id(1)), &peer(&id(1))).0, LeaseOutcome::Closing);
}

#[test]
fn cleanup_finish_never_ends_a_shutdown() {
    let f = restart(&HeldRecord { holders: vec![id(1)], handover_until_ms: Some(T0), ..empty() });
    f.leases.begin_close();
    f.leases.finish_cleanup(0, vec![]).unwrap();
    assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "closing until the shutdown's own step 5");
    assert_eq!(f.leases.grant(&req(&id(2)), &peer(&id(2))).0, LeaseOutcome::Closing);
}

#[tokio::test]
async fn gone_wakes_every_waiter() {
    let f = fixture(Some(BOOT));
    let waiters: Vec<_> = (0..2)
        .map(|_| {
            let leases = f.leases.clone();
            tokio::spawn(async move { leases.gone().await })
        })
        .collect();
    tokio::task::yield_now().await;
    f.leases.begin_close();
    for w in waiters {
        tokio::time::timeout(Duration::from_secs(1), w).await.expect("every waiter sees the shutdown").unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), f.leases.gone()).await.expect("a late waiter too");
}

#[test]
fn notice_clears_only_on_matching_ack() {
    let f = fixture(Some(BOOT));
    f.leases.finish_cleanup(2, vec![]).unwrap();
    assert_eq!(f.leases.notice(), 2);
    assert_eq!(f.on_disk(), Some(HeldRecord { not_ended: 2, ..empty() }));
    f.grant(&id(1));
    assert_eq!(f.leases.notice(), 2, "a grant does not ack it");
    for stale in [0, 1, 3] {
        f.leases.notice_seen(stale).unwrap();
        assert_eq!(f.leases.notice(), 2, "an ack of {stale}");
    }
    f.leases.notice_seen(2).unwrap();
    assert_eq!(f.leases.notice(), 0);
    assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(1)], ..empty() }));
}

#[test]
fn boots_match_table() {
    for (req, own, strict, lenient) in [
        ("80", "80", true, true),
        ("80", "81", false, false),
        ("80", "", false, true),
        ("", "80", false, true),
        ("", "81", false, true),
        ("", "", true, true),
    ] {
        assert_eq!(boots_match(req, own, false), strict, "strict {req:?} {own:?}");
        assert_eq!(boots_match(req, own, true), lenient, "lenient {req:?} {own:?}");
    }
}

#[test]
fn startup_plan_boot_row_compares_only_two_nonempty_boots() {
    use StartPlan::*;
    let with = |boot: &str| Ok(Some(HeldRecord { boot: boot.into(), ..empty() }));
    for (name, rec_boot, own, want) in [
        ("80 vs 81", "80", Ok("81"), Cleanup),
        ("80 vs unknown", "80", Ok(""), Resume),
        ("unknown vs 81", "", Ok("81"), Resume),
        ("unknown vs unknown", "", Ok(""), Resume),
        ("own Err", "80", Err(()), Cleanup),
    ] {
        assert_eq!(startup_plan(&with(rec_boot), own, T0), want, "{name}");
    }
}

#[test]
fn startup_plan_table() {
    use StartPlan::*;
    let _takes_no_liveness: fn(&Result<Option<HeldRecord>, String>, Result<&str, ()>, u64) -> StartPlan = startup_plan;
    let rec = |edit: fn(&mut HeldRecord)| {
        let mut r = empty();
        edit(&mut r);
        Ok(Some(r))
    };
    let future = T0 + 5;
    let cases: Vec<(&str, Result<Option<HeldRecord>, String>, StartPlan)> = vec![
        ("absent", Ok(Option::None), Resume),
        ("unreadable", Err("unreadable".into()), Cleanup),
        ("closing", rec(|r| r.closing = true), Cleanup),
        ("closing, with holders", rec(|r| (r.closing, r.holders) = (true, vec![id(1)])), Cleanup),
        ("other boot", rec(|r| r.boot = "boot-b".into()), Cleanup),
        ("other boot, future handover", rec(|r| (r.boot, r.handover_until_ms) = ("boot-b".into(), Some(T0 + 5))), Cleanup),
        ("other boot, holders", rec(|r| (r.boot, r.holders) = ("boot-b".into(), vec![id(1)])), Cleanup),
        ("handover at its deadline", rec(|r| r.handover_until_ms = Some(T0)), Cleanup),
        ("handover passed", rec(|r| r.handover_until_ms = Some(T0 - 1)), Cleanup),
        ("handover in the future", rec(|r| r.handover_until_ms = Some(T0 + 5)), Pending { until_ms: future }),
        ("holders", rec(|r| r.holders = vec![id(1)]), Pending { until_ms: T0 + handover_bound_ms() }),
        ("holders, handover passed", rec(|r| (r.holders, r.handover_until_ms) = (vec![id(1)], Some(T0 - 1))), Cleanup),
        ("holders, handover in the future", rec(|r| (r.holders, r.handover_until_ms) = (vec![id(1)], Some(T0 + 5))), Pending { until_ms: future }),
        ("only not_ended and forget", rec(|r| (r.not_ended, r.forget) = (2, vec!["w".into()])), Resume),
    ];
    for (name, read, want) in &cases {
        assert_eq!(startup_plan(read, Ok(BOOT), T0), *want, "{name}");
        let unknown = if matches!(read, Ok(Option::None)) { Resume } else { Cleanup };
        assert_eq!(startup_plan(read, Err(()), T0), unknown, "{name}, own boot unknown");
        if !matches!(read, Ok(Some(r)) if r.boot != BOOT) {
            let windows = read.clone().map(|r| r.map(|r| HeldRecord { boot: String::new(), ..r }));
            assert_eq!(startup_plan(&windows, Ok(""), T0), *want, "{name}, \"\" = \"\"");
        }
    }
}

#[test]
fn shutdown_forgets_the_restart_pending() {
    let f = restart(&HeldRecord { holders: vec![id(1), id(2)], ..empty() });
    f.install_planned(T0);
    f.leases.begin_close();
    f.leases.finish_shutdown(0, vec![]).unwrap();
    let rec = f.on_disk();
    assert!(rec.as_ref().map_or(true, |r| r.holders.is_empty() && r.handover_until_ms.is_none()), "{rec:?}");
    assert_eq!(startup_plan(&read_record(&f.path), Ok(BOOT), T0 + 1), StartPlan::Resume);
}

#[test]
fn restart_with_holders_any_lease_clears_the_pending() {
    let (a, b) = (id(1), id(2));
    let f = restart(&HeldRecord { holders: vec![a], ..empty() });
    let until = f.install_planned(T0);
    f.grant(&b);
    assert_eq!(f.leases.tick(until), Tick::None, "any granted lease clears the restart's pending: its deadline decides nothing");
}

#[test]
fn lease_granted_before_install_pending_still_counts() {
    let a = id(1);
    let f = restart(&HeldRecord { holders: vec![a.clone()], ..empty() });
    f.grant(&a);
    let until = f.install_planned(T0);
    assert_eq!(f.leases.tick(until), Tick::None, "a lease held when the pending was installed counts: its deadline decides nothing");
    assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![a], ..empty() }), "the pending is cleared");
}

#[test]
fn close_after_pending_clears_is_immediate() {
    let (a, b) = (id(1), id(2));
    let f = restart(&HeldRecord { holders: vec![a.clone(), b], ..empty() });
    f.install_planned(T0);
    let gen = f.grant(&a);
    assert_eq!(
        f.leases.depart(gen, Some(LeaveIntent::Close), T0 + 1),
        Decision::Shutdown,
        "the last lease's Close after the pending cleared is a shutdown at once, never deferred for a recorded holder"
    );
    assert!(f.on_disk().expect("the record").closing);
}

#[test]
fn pending_deadline_is_persisted_once() {
    let f = restart(&HeldRecord { holders: vec![id(1)], ..empty() });
    let until = f.install_planned(T0);
    assert_eq!(
        f.on_disk().map(|r| r.handover_until_ms),
        Some(Some(until)),
        "install_pending writes its deadline as handover_until_ms"
    );
    for now in [T0 + 10_000, until - 1] {
        let plan = startup_plan(&read_record(&f.path), Ok(BOOT), now);
        assert!(matches!(plan, StartPlan::Pending { until_ms, .. } if until_ms == until), "a restart at {now} keeps the deadline: {plan:?}");
    }
    assert_eq!(startup_plan(&read_record(&f.path), Ok(BOOT), until), StartPlan::Cleanup, "a restart at the deadline");
}

#[test]
fn unreadable_record_cleanup_keeps_the_record_on_grant() {
    let f = restart_raw("{not json");
    f.grant(&id(1));
    assert_eq!(
        std::fs::read_to_string(&f.path).unwrap(),
        "{not json",
        "a grant during an unreadable record's Cleanup rewrote the record"
    );
    f.leases.finish_cleanup(0, vec![]).unwrap();
    assert_eq!(f.on_disk(), Some(HeldRecord { holders: vec![id(1)], ..empty() }), "recorded once the Cleanup finishes");
}

#[test]
fn keep_during_startup_cleanup_keeps_the_record() {
    let passed = HeldRecord { holders: vec![id(9)], handover_until_ms: Some(T0 - 1), ..empty() };
    for intent in [LeaveIntent::Keep, LeaveIntent::Handover] {
        let f = restart(&passed);
        let gen = f.grant(&id(1));
        f.leases.depart(gen, Some(intent), T0);
        assert_eq!(f.on_disk(), Some(passed.clone()), "a {intent:?} during the startup Cleanup deleted or rewrote the record");
    }
}

#[test]
fn close_during_startup_cleanup_writes_closing() {
    let f = restart_raw("{not json");
    let gen = f.grant(&id(1));
    assert_eq!(f.leases.depart(gen, Some(LeaveIntent::Close), T0), Decision::Shutdown);
    assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "a last Close during the startup Cleanup");
    let f = restart_raw("{not json");
    f.leases.begin_close();
    assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "begin_close during the startup Cleanup");
}

#[test]
fn record_roundtrip_and_empty_file_is_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(bounds::HELD_RECORD_FILE);
    assert_eq!(read_record(&path), Ok(Option::None));
    let full = HeldRecord {
        holders: vec![id(1)],
        handover_until_ms: Some(T0),
        closing: true,
        not_ended: 3,
        forget: vec!["w1".into()],
        ..empty()
    };
    write_or_delete(&path, &full).unwrap();
    assert_eq!(read_record(&path), Ok(Some(full.clone())));
    write_or_delete(&path, &HeldRecord { handover_until_ms: None, ..full }).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"v":1,"boot":"boot-a","holders":[{"boot":"boot-a","pid":1,"created":7001}],"handover_until_ms":null,"closing":true,"not_ended":3,"forget":["w1"]}"#
    );
    let keeps: [(&str, fn(&mut HeldRecord)); 5] = [
        ("holders", |r| r.holders = vec![id(1)]),
        ("handover", |r| r.handover_until_ms = Some(T0)),
        ("closing", |r| r.closing = true),
        ("not_ended", |r| r.not_ended = 1),
        ("forget", |r| r.forget = vec!["w".into()]),
    ];
    for (name, edit) in keeps {
        let mut r = empty();
        edit(&mut r);
        write_or_delete(&path, &r).unwrap();
        assert_eq!(read_record(&path), Ok(Some(r)), "{name} alone keeps the file");
    }
    write_or_delete(&path, &empty()).unwrap();
    assert!(!path.exists(), "an empty record is deleted");
    write_or_delete(&path, &empty()).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no temporary file is left");
    for bad in ["{not json", r#"{"v":2,"boot":"","holders":[],"closing":false,"not_ended":0,"forget":[]}"#, r#"{"v":1}"#] {
        std::fs::write(&path, bad).unwrap();
        assert!(read_record(&path).is_err(), "{bad}");
    }
}

#[cfg(unix)]
#[test]
fn record_write_syncs_its_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(bounds::HELD_RECORD_FILE);
    let mode = |m: u32| std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(m)).unwrap();
    let full = HeldRecord { holders: vec![id(1)], ..empty() };
    // Write and search but no read: the rename and the delete succeed,
    // and only the directory's open for its sync fails.
    mode(0o300);
    assert!(write_or_delete(&path, &full).is_err(), "a write whose directory cannot be synced");
    mode(0o700);
    write_or_delete(&path, &full).unwrap();
    mode(0o300);
    assert!(write_or_delete(&path, &empty()).is_err(), "a delete whose directory cannot be synced");
    mode(0o700);
}

/// One request line, as a window writes it.
fn line(id: u64, op: &str, payload: impl Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&Frame::req(id, op, serde_json::to_value(payload).unwrap())).unwrap();
    bytes.push(b'\n');
    bytes
}

/// `hold` for `who`'s `fe.lease` over one end of a duplex.
async fn hold_over(ours: tokio::io::DuplexStream, who: ProcessIdentity, leases: &Leases) -> anyhow::Result<()> {
    let (rx, tx) = tokio::io::split(ours);
    let first = Frame::req(1, op::FE_LEASE, serde_json::to_value(req(&who)).unwrap());
    hold(tokio::io::BufReader::new(rx), tx, first, peer(&who), leases, None).await
}

#[tokio::test]
async fn failed_grant_reply_still_departs() {
    use tokio::io::AsyncWriteExt as _;
    // The window is gone before its grant's reply is written.
    let f = fixture(Some(BOOT));
    let (ours, theirs) = tokio::io::duplex(4096);
    drop(theirs);
    assert!(hold_over(ours, id(1), &f.leases).await.is_err(), "the grant's reply was written");
    assert_eq!(f.on_disk(), Some(HeldRecord { closing: true, ..empty() }), "a failed grant reply left its lease held");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), f.leases.gone()).await.is_ok(),
        "the last lease's failed grant reply decided no shutdown"
    );

    // A Keep received, then its reply fails: the departure stays a Keep.
    let f = fixture(Some(BOOT));
    let (ours, theirs) = tokio::io::duplex(4096);
    let window = async move {
        let mut theirs = tokio::io::BufReader::new(theirs);
        let mut grant = String::new();
        theirs.read_line(&mut grant).await.unwrap();
        theirs.get_mut().write_all(&line(2, op::FE_LEAVING, FeLeavingReq { intent: LeaveIntent::Keep })).await.unwrap();
    };
    let (held, ()) = tokio::join!(hold_over(ours, id(1), &f.leases), window);
    assert!(held.is_err(), "the Keep's reply was written");
    assert_eq!(f.on_disk(), None, "a failed write after a Keep departed as a close");
    assert!(tokio::time::timeout(Duration::from_millis(50), f.leases.gone()).await.is_err(), "a Keep shut down");
}

#[tokio::test]
async fn closer_is_told_the_whole_count() {
    use tokio::io::AsyncWriteExt as _;
    let f = restart(&HeldRecord { not_ended: 3, ..empty() });
    let leases = &f.leases;
    let (ours, theirs) = tokio::io::duplex(4096);
    let window = async move {
        let mut theirs = tokio::io::BufReader::new(theirs);
        let mut text = String::new();
        theirs.read_line(&mut text).await.unwrap();
        theirs.get_mut().write_all(&line(2, op::FE_LEAVING, FeLeavingReq { intent: LeaveIntent::Close })).await.unwrap();
        leases.gone().await;
        leases.finish_shutdown(2, Vec::new()).unwrap();
        text.clear();
        theirs.read_line(&mut text).await.unwrap();
        serde_json::from_str::<Frame>(&text).unwrap().payload["not_ended"].clone()
    };
    let (_, told) = tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(hold_over(ours, id(1), leases), window) })
        .await
        .expect("the close was not answered");
    assert_eq!(told, serde_json::json!(5), "the closer was told only this shutdown's count, not the record's");
}

/// For every ordered pair of intents on the last lease, the second
/// decides (ruling c). A `Close` first is a shutdown at once, and its
/// lease is answered and ended, so no later intent is read.
#[tokio::test]
async fn latest_intent_wins_table() {
    use tokio::io::AsyncWriteExt as _;
    use LeaveIntent::*;
    let mut wrong = Vec::new();
    for first in [Keep, Handover, Close] {
        for second in [Keep, Handover, Close] {
            let f = fixture(Some(BOOT));
            let leases = &f.leases;
            let (ours, theirs) = tokio::io::duplex(4096);
            let window = async move {
                let mut theirs = tokio::io::BufReader::new(theirs);
                let mut text = String::new();
                theirs.read_line(&mut text).await.unwrap();
                for (id, intent) in [(2, first), (3, second)] {
                    if theirs.get_mut().write_all(&line(id, op::FE_LEAVING, FeLeavingReq { intent })).await.is_err() {
                        break;
                    }
                    text.clear();
                    if theirs.read_line(&mut text).await.unwrap_or(0) == 0 {
                        break;
                    }
                }
            };
            // Shutdown step 5, so a deciding close is answered.
            let finish = async {
                leases.gone().await;
                leases.finish_shutdown(0, Vec::new()).unwrap();
                std::future::pending::<()>().await;
            };
            let before = now_ms();
            tokio::time::timeout(Duration::from_secs(10), async {
                tokio::select! {
                    _ = async { tokio::join!(hold_over(ours, id(1), leases), window) } => {}
                    () = finish => {}
                }
            })
            .await
            .expect("the window's intents were not answered");
            let bounded = before + handover_bound_ms()..=now_ms() + handover_bound_ms();
            let shut = tokio::time::timeout(Duration::from_millis(50), leases.gone()).await.is_ok();
            let got = match (shut, f.on_disk().and_then(|rec| rec.handover_until_ms)) {
                (true, _) => "a shutdown",
                (false, Some(until)) if bounded.contains(&until) => "a bounded handover",
                (false, Some(_)) => "a handover from another moment",
                (false, None) => "an open-ended keep",
            };
            let want = match if first == Close { first } else { second } {
                Close => "a shutdown",
                Keep => "an open-ended keep",
                Handover => "a bounded handover",
            };
            if got != want {
                wrong.push(format!("{first:?} then {second:?}: {got}, not {want}"));
            }
        }
    }
    assert!(wrong.is_empty(), "the latest intent on a lease did not win: {wrong:?}");
}

/// The pending's deadline has passed but no tick has acted on it: a
/// grant still clears it, so a later Keep stays open-ended.
#[test]
fn grant_after_deadline_before_tick_clears_the_pending() {
    let until = T0 + handover_bound_ms();
    let f = fixture(Some(BOOT));
    f.leases.install_pending(until).unwrap();
    let (outcome, gen) = f.leases.grant(&req(&id(1)), &peer(&id(1)));
    assert_eq!(outcome, LeaseOutcome::Granted);
    assert_eq!(f.leases.depart(gen.unwrap(), Some(LeaveIntent::Keep), until + 1), Decision::None);
    assert_eq!(
        f.leases.tick(until + 2),
        Tick::None,
        "a grant past the deadline but before the tick left the pending armed: the Keep was shut down"
    );
    assert_eq!(f.on_disk(), None, "the pending is cleared");
}
