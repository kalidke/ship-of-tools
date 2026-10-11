//! Display-free behavioral proofs of the paths used by the native drivers.

use super::*;

#[test]
#[cfg(feature = "test-pane-timing")]
fn pane_child_layout_has_agent_at_any_monitor_shape() {
    use crate::ui::persist::settings::Slot;
    let settings = super::super::native_pane_tests::pane_child_settings();
    for aspect in [4.0 / 3.0, 0.75, 1.5, 1.6] {
        assert!(settings.resolve_preset(aspect).columns.contains(&Slot::Llm),
            "pane child has no agent column at aspect {aspect}");
    }
    assert!(!Settings::default().resolve_preset(0.75).columns.contains(&Slot::Llm));
}

#[test]
fn native_drivers_ignore_unmarked_wakes() {
    use super::super::native_children::HarnessTick;
    let tick = HarnessTick::default();
    let mut trace = Vec::new();
    for _ in 0..100 {
        tick.dispatch(&mut trace, |t| t.push("forward"), |t, _| t.push("work"));
    }
    assert_eq!(trace.len(), 100, "unmarked wakes advanced harness work");
    tick.post();
    tick.dispatch(&mut trace, |t| t.push("forward"), |t, _| t.push("work"));
    assert_eq!(&trace[100..], &["forward", "work"]);
    for _ in 0..10 {
        tick.dispatch(&mut trace, |t| t.push("forward"), |t, _| t.push("work"));
    }
    assert_eq!(trace.iter().filter(|&&v| v == "work").count(), 1);
}

#[test]
fn pending_tick_is_consumed_once() {
    use super::super::native_children::HarnessTick;
    let tick = HarnessTick::default();
    let mut observed = (0, 0, 0); // forwards, work calls, consumed posts
    for _ in 0..3 { tick.post(); }
    for _ in 0..2 {
        tick.dispatch(&mut observed, |o| o.0 += 1, |o, count| { o.1 += 1; o.2 += count; });
    }
    assert_eq!(observed.0, 2);
    assert_eq!(observed.1, 1, "coalesced tick advanced twice");
    assert_eq!(observed.2, 3, "pending tick count was lost: consumed={} expected=3", observed.2);
    tick.post();
    tick.dispatch(&mut observed, |o| o.0 += 1, |o, count| { o.1 += 1; o.2 += count; tick.post(); });
    for _ in 0..2 {
        tick.dispatch(&mut observed, |o| o.0 += 1, |o, count| { o.1 += 1; o.2 += count; });
    }
    assert_eq!(observed, (5, 3, 5), "post during consumed work was lost or duplicated");
}

#[test]
fn coalesced_progress_ticks_preserve_sample_count() {
    use super::super::native_children::HarnessTick;
    let (mut ledger, epoch) = seeded(MINIMIZED, &scheduled(MINIMIZED), true);
    ledger.phases.get_mut(MINIMIZED.phase).unwrap().samples = 598;
    let ledger = Mutex::new(ledger);
    let tick = HarnessTick::default();
    tick.post();
    tick.post();
    let mut reports = Vec::new();
    tick.dispatch(&mut reports, |_| {}, |reports, count| {
        reports.push(report_progress(&ledger, ReportRequest::Sample(MINIMIZED,
            epoch + Duration::from_secs(600), true, count), false, &mut Vec::new()).unwrap());
    });
    let report = &reports[0];
    assert!(report.completed);
    assert_eq!(report.snapshot.samples, 600,
        "coalesced progress ticks lost sample credit: samples={} expected=600", report.snapshot.samples);
    assert!(report.failures.is_empty(), "valid coalesced workload failed: {:?}", report.failures);
    tick.dispatch(&mut reports, |_| {}, |_, _| panic!("unmarked dispatch added sample credit"));
    assert_eq!(reports.len(), 1);
    assert_eq!(ledger.lock().unwrap().phases[MINIMIZED.phase].samples, 600);
    let report = report_progress(&ledger, ReportRequest::Begin(CONTROL, true), false, &mut Vec::new()).unwrap();
    assert_eq!(report.snapshot.samples, 0, "new phase inherited ticker credit");
    let report = report_progress(&ledger, ReportRequest::Sample(MINIMIZED,
        epoch + Duration::from_secs(600), false, 2), false, &mut Vec::new()).unwrap();
    assert_eq!(report.snapshot.samples, 602);
    assert_eq!(report.failures, ["native_observation"], "ticker credit hid the wrong native state");
}

#[test]
fn progress_reports_include_sample_counts() {
    let ledger = Mutex::new(NativeProgressLedger::default());
    let mut bytes = Vec::new();
    let report = report_progress(&ledger, ReportRequest::Begin(VISIBLE, false), false, &mut bytes).unwrap();
    assert_report_samples(&bytes, &report);
    assert_eq!(report.snapshot.samples, 0);
    for (profile, complete, mutation) in [
        (VISIBLE, false, false), (VISIBLE, true, false), (CONTROL, true, false),
        (MINIMIZED, true, false), (VISIBLE, true, true),
    ] {
        let mut events = scheduled(profile);
        if mutation { events[0].1 = Duration::from_millis(301); }
        let (mut ledger, epoch) = seeded(profile, &events, true);
        if !complete { ledger.phases.get_mut(profile.phase).unwrap().producer_done = None; }
        let ledger = Mutex::new(ledger);
        let mut bytes = Vec::new();
        let report = report_progress(&ledger, ReportRequest::Sample(profile,
            epoch + Duration::from_secs(profile.seconds as u64), profile.minimized, 1), mutation, &mut bytes).unwrap();
        assert_eq!(report.completed, complete);
        assert_report_samples(&bytes, &report);
        if profile.phase == MINIMIZED.phase {
            let text = String::from_utf8(bytes).unwrap();
            let line = text.lines().find(|line| line.starts_with("window-progress phase=completed ")).unwrap();
            assert!(line.split_whitespace().any(|field| field == format!("samples={}", report.snapshot.samples)),
                "minimized completion omitted samples");
        }
    }
}

fn assert_report_samples(bytes: &[u8], report: &ProgressReport) {
    let text = std::str::from_utf8(bytes).unwrap();
    let snapshots: Vec<_> = text.lines().filter(|line| line.starts_with("window-progress test=")).collect();
    assert_eq!(snapshots.len(), if report.completed { 2 } else { 1 });
    for line in snapshots {
        let phase = line.split_whitespace().find(|field| field.starts_with("phase=")).unwrap();
        assert!(line.split_whitespace().any(|field| field == format!("samples={}", report.snapshot.samples)),
            "progress snapshot omitted samples: {phase}");
    }
}

fn seeded(profile: Profile, events: &[(usize, Duration)], dequeue: bool) -> (NativeProgressLedger, Instant) {
    let mut ledger = NativeProgressLedger::default();
    let epoch = ledger.begin(profile);
    for &(seq, elapsed) in events {
        ledger.accepted_at(Some((profile.phase.to_string(), seq)), epoch + elapsed);
        if dequeue {
            ledger.dequeued_at(profile.phase.to_string(), seq, epoch + elapsed);
        }
    }
    let phase = ledger.phases.get_mut(profile.phase).unwrap();
    phase.samples = profile.seconds as usize;
    phase.producer_done = Some(epoch + Duration::from_secs(profile.seconds as u64));
    (ledger, epoch)
}

fn scheduled(profile: Profile) -> Vec<(usize, Duration)> {
    (1..=profile.expected).filter(|seq| !(profile.stall && (100..200).contains(seq)))
        .map(|seq| (seq, Duration::from_millis(seq as u64 * 50))).collect()
}

fn interval_oracle(profile: Profile, events: &[(usize, Duration)], elapsed: Duration) -> (usize, usize, u128) {
    let seconds = elapsed.as_secs().min(profile.seconds as u64);
    let windows: Vec<_> = (0..=seconds.saturating_sub(10)).filter(|_| seconds >= 10)
        .map(|offset| events.iter().filter(|(_, t)| {
            *t >= Duration::from_secs(offset) && *t < Duration::from_secs(offset + 10)
        }).count()).collect();
    let late = events.iter().map(|(seq, time)| time.saturating_sub(Duration::from_millis(*seq as u64 * 50)).as_millis())
        .max().unwrap_or(0);
    (windows.iter().copied().min().unwrap_or(0), windows.iter().copied().max().unwrap_or(0), late)
}

#[test]
fn accepted_second_bins_match_interval_oracle() {
    let mut random = 23_u64;
    for seconds in [0, 2, 9, 10, 11, 20, 600] {
        let profile = Profile { seconds, expected: 100, ..VISIBLE };
        let mut events = vec![(1, Duration::ZERO), (2, Duration::from_nanos(999_999_999)),
            (3, Duration::from_secs(1)), (4, Duration::from_nanos(9_999_999_999)),
            (5, Duration::from_secs(10)), (6, Duration::from_secs(seconds as u64)),
            (7, Duration::from_secs(seconds as u64 + 1))];
        for seq in 8..=100 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            events.push((seq, Duration::from_millis(random % ((seconds as u64 + 3) * 1000))));
        }
        events.reverse(); // Sequence order and accepted-time order are independent.
        let (ledger, epoch) = seeded(profile, &events, false);
        for elapsed in [Duration::ZERO, Duration::from_secs(9), Duration::from_secs(10),
            Duration::from_secs(seconds as u64), Duration::from_secs(seconds as u64 + 20)]
        {
            assert_eq!(ledger.phases[profile.phase].snapshot(epoch + elapsed, false).rate,
                interval_oracle(profile, &events, elapsed), "seconds={seconds} elapsed={elapsed:?}");
        }
    }
    let events = scheduled(MINIMIZED);
    let (ledger, epoch) = seeded(MINIMIZED, &events, false);
    assert_eq!(ledger.phases[MINIMIZED.phase].snapshot(epoch + Duration::from_secs(620), true).rate,
        (199, 200, 0), "the 600-second endpoint belongs to no scheduled window");
}

#[test]
fn workload_validator_rejects_stalls_and_late_sends() {
    for profile in [VISIBLE, CONTROL, MINIMIZED] {
        let (mut ledger, epoch) = seeded(profile, &scheduled(profile), true);
        let phase = ledger.phases.get_mut(profile.phase).unwrap();
        let now = epoch + Duration::from_secs(profile.seconds as u64);
        let failures = phase.snapshot(now, profile.minimized).failures();
        if profile.stall {
            assert_eq!(failures, ["workload_volume", "workload_rate"]);
        } else {
            assert!(failures.is_empty(), "valid scheduled workload failed: {failures:?}");
        }
        phase.native_ok = false;
        assert!(phase.snapshot(now, profile.minimized).failures().contains(&"native_observation"));
        phase.native_ok = true;
        phase.samples = 0;
        assert!(phase.snapshot(now, profile.minimized).failures().contains(&"native_observation"));
        phase.samples = profile.seconds as usize;
        assert!(phase.snapshot(epoch, profile.minimized).failures().contains(&"native_observation"));
        phase.producer_done = None;
        assert!(phase.snapshot(now, profile.minimized).failures().contains(&"final_drain"));
    }
    let mut events = scheduled(VISIBLE);
    events[0].1 = Duration::from_millis(301);
    let (mut ledger, epoch) = seeded(VISIBLE, &events, true);
    assert_eq!(ledger.phases[VISIBLE.phase].snapshot(epoch + Duration::from_secs(2), false).failures(), ["workload_rate"]);
    ledger.phases.get_mut(VISIBLE.phase).unwrap().producer_done = Some(epoch - Duration::from_secs(4));
    assert!(ledger.phases[VISIBLE.phase].snapshot(epoch + Duration::from_secs(2), false).failures().contains(&"final_drain"));
    let events = scheduled(CONTROL);
    let (mut ledger, epoch) = seeded(CONTROL, &events, false);
    assert!(ledger.phases[CONTROL.phase].snapshot(epoch + Duration::from_secs(20), true).failures().contains(&"queue_progress"));
    use std::panic::{catch_unwind, AssertUnwindSafe};
    assert!(catch_unwind(AssertUnwindSafe(|| ledger.accepted_at(Some((CONTROL.phase.into(), 1)), epoch))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| ledger.accepted_at(Some((CONTROL.phase.into(), 401)), epoch))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| ledger.dequeued_at(CONTROL.phase.into(), 100, epoch))).is_err());
    ledger.dequeued_at(CONTROL.phase.into(), 1, epoch);
    assert!(catch_unwind(AssertUnwindSafe(|| ledger.dequeued_at(CONTROL.phase.into(), 1, epoch))).is_err());
}

#[test]
fn full_phase_sampling_visits_seconds_not_events() {
    let (ledger, epoch) = seeded(MINIMIZED, &scheduled(MINIMIZED), false);
    let phase = &ledger.phases[MINIMIZED.phase];
    phase.timing_visits.set(0);
    let snapshot = phase.snapshot(epoch + Duration::from_secs(600), true);
    assert_eq!(snapshot.accepted, 12000);
    let visits = phase.timing_visits.get();
    let bound = 2 * MINIMIZED.seconds as usize;
    assert!(visits <= bound, "summary visited {visits} timing elements; bound is {bound}");
    println!("native-harness summary timing_visits={visits} bound={bound} events=12000");
}

struct LockWriter<'a> {
    ledger: &'a Mutex<NativeProgressLedger>,
    bytes: Vec<u8>,
    writes: usize,
    flushes: usize,
}

impl Write for LockWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        assert!(self.ledger.try_lock().is_ok(), "progress writer holds ledger during write");
        self.writes += 1;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        assert!(self.ledger.try_lock().is_ok(), "progress writer holds ledger during flush");
        self.flushes += 1;
        Ok(())
    }
}

fn check_report_paths_release_lock() {
    for (profile, complete, mutation, wanted) in [
        (VISIBLE, false, false, "phase=sample"),
        (VISIBLE, true, false, "phase=completed"),
        (CONTROL, true, false, "observed_failure=workload_volume,workload_rate control_bodies=1"),
        (MINIMIZED, true, false, "workload_ok=true completed_bodies=1"),
        (VISIBLE, true, true, "mac-redraw-mutation visible failures=workload_rate"),
    ] {
        let mut events = scheduled(profile);
        if mutation { events[0].1 = Duration::from_millis(301); }
        let (mut ledger, epoch) = seeded(profile, &events, true);
        if !complete { ledger.phases.get_mut(profile.phase).unwrap().producer_done = None; }
        let ledger = Mutex::new(ledger);
        let mut writer = LockWriter { ledger: &ledger, bytes: Vec::new(), writes: 0, flushes: 0 };
        let report = report_progress(&ledger, ReportRequest::Sample(profile,
            epoch + Duration::from_secs(profile.seconds as u64), profile.minimized, 1), mutation, &mut writer).unwrap();
        assert_eq!(report.completed, complete);
        assert!(String::from_utf8(writer.bytes).unwrap().contains(wanted));
        assert!(writer.writes > 0 && writer.flushes > 0);
    }
}

struct BlockingWriter {
    entered: Option<std::sync::mpsc::SyncSender<()>>,
    release: std::sync::mpsc::Receiver<()>,
    bytes: Vec<u8>,
}

impl Write for BlockingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(entered) = self.entered.take() {
            entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(2)).expect("bounded output release");
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn blocked_output_allows_producer() {
    let ledger = Arc::new(Mutex::new(NativeProgressLedger::default()));
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let reporter_ledger = ledger.clone();
    let reporter = std::thread::spawn(move || {
        let mut writer = BlockingWriter { entered: Some(entered_tx), release: release_rx, bytes: Vec::new() };
        report_progress(&reporter_ledger, ReportRequest::Begin(VISIBLE, false), false, &mut writer).unwrap();
        writer.bytes
    });
    entered_rx.recv_timeout(Duration::from_secs(2)).expect("writer entered");
    let (producer_tx, producer_rx) = std::sync::mpsc::sync_channel(1);
    let producer = std::thread::spawn(move || {
        let mut ledger = ledger.lock().unwrap();
        let epoch = ledger.phases[VISIBLE.phase].epoch;
        ledger.accepted_at(Some((VISIBLE.phase.into(), 1)), epoch + Duration::from_millis(50));
        producer_tx.send(()).unwrap();
    });
    let acquired = producer_rx.recv_timeout(Duration::from_secs(1));
    release_tx.send(()).unwrap();
    let bytes = reporter.join().unwrap();
    producer.join().unwrap();
    assert!(acquired.is_ok(), "blocked writer prevented producer ledger acquisition");
    assert!(String::from_utf8(bytes).unwrap().contains("phase=body-entered"));
}

#[test]
fn progress_output_releases_ledger_before_write() {
    let ledger = Mutex::new(NativeProgressLedger::default());
    let mut writer = LockWriter { ledger: &ledger, bytes: Vec::new(), writes: 0, flushes: 0 };
    report_progress(&ledger, ReportRequest::Begin(VISIBLE, false), false, &mut writer).unwrap();
    assert!(String::from_utf8(writer.bytes).unwrap().contains("phase=body-entered"));
    assert!(writer.writes > 0 && writer.flushes > 0);
    check_report_paths_release_lock();
    blocked_output_allows_producer();
}
