//! Capsule tests for the early end of the output stream, driven by a fake producer.

use super::*;

// ---------------------------------------------------------------------
// ADR 0043 decision 12, as amended (the macOS pty revoke): the pre-close
// end of the output stream. `capsule::run` is generic over `Producer`, so
// the orderings this rule has to get right — which are a KERNEL race
// between a session leader's exit and its controlling terminal's revoke on
// the platform that actually has them — are driven here from a fake
// producer instead, deterministically, on whatever platform runs the
// suite. The four tests below are named for the invariant, not for the
// platform: "the output stream may end before `close_output_side` only as
// a consequence of the producer's own exit; a terminal reader event that a
// bounded confirmation of that exit does not explain is capsule-fatal".
// ---------------------------------------------------------------------

/// What the fake producer's output side does when its scripted moment
/// arrives: end cleanly (the reader turns this into `Done(Ok(()))`), or
/// panic inside `read` (the reader's drop guard turns that into
/// `ReaderGone` with no terminal `Done` at all).
#[derive(Clone, Copy, PartialEq)]
enum OutputEnd {
    Eof,
    PanicInRead,
}

/// When the fake producer's exit becomes OBSERVABLE to `wait` — the whole
/// point of the fake, since this is the instant a real kernel does not let
/// a test choose.
#[derive(Clone, Copy)]
enum ExitObservable {
    /// Already exited before the writer loop's first poll (ordering B: the
    /// loop's own `wait(ZERO)` wins the race).
    AtSpawn,
    /// This long after the capsule starts confirming an exit (its first
    /// `wait` with a timeout above zero; ordering A: the reader wins, and
    /// the confirmation has to block out the remainder). Keyed on the
    /// capsule's own call, not on the script's clock, so no scheduling
    /// delay can make the exit observable before the capsule asks.
    AfterConfirmationStarts(Duration),
    /// Never — the producer is alive past the grace, which is the case the
    /// rule still has to kill the capsule for.
    Never,
}

/// When `domain_is_empty` first answers "empty".
#[derive(Clone, Copy)]
enum Domain {
    /// As soon as the producer has exited: a domain already down to an
    /// unreaped leader zombie.
    EmptyOnceExited,
    /// Only once the capsule has confirmed the exit after a terminal reader
    /// event: a domain that still has a descendant to reap when teardown
    /// starts, so Phase A keeps servicing the channel until the terminal
    /// event has been taken.
    EmptyOnceTheExitIsConfirmed,
}

struct FakeScript {
    output_ends_after: Duration,
    output_end: OutputEnd,
    exit: ExitObservable,
    domain: Domain,
}

/// The script `FakeProducer::spawn` picks up — `Producer::spawn` is an
/// associated function with nowhere for a test to hand it anything, so the
/// handoff is a process-global that every test using it holds `serial()`
/// across.
static FAKE_SCRIPT: std::sync::Mutex<Option<FakeScript>> = std::sync::Mutex::new(None);

struct FakeState {
    exit_at: std::sync::Mutex<Option<Instant>>,
    exit_after_confirming: Option<Duration>,
    /// Set by the first `wait` with a timeout above zero: the capsule
    /// confirming an exit after a terminal reader event (both `Done` arms
    /// call `wait(READER_END_EXIT_GRACE)`; the main loop polls with zero).
    confirming: AtomicBool,
    domain: Domain,
}

impl FakeState {
    fn exited(&self) -> bool {
        matches!(*self.exit_at.lock().unwrap(), Some(t) if Instant::now() >= t)
    }
}

/// The fake's output side: it blocks (a quiet producer with nothing to
/// say) until the script's thread tells it how this stream ends.
struct FakeOutput {
    steps: mpsc::Receiver<OutputEnd>,
}

impl std::io::Read for FakeOutput {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        match self.steps.recv() {
            // A dropped sender is `close_output_side` having taken the
            // output side away: an ordinary end of stream.
            Ok(OutputEnd::Eof) | Err(_) => Ok(0),
            Ok(OutputEnd::PanicInRead) => panic!("fake producer: scripted panic inside read()"),
        }
    }
}

struct FakeProducer {
    state: Arc<FakeState>,
    output: Option<FakeOutput>,
    steps: Option<mpsc::Sender<OutputEnd>>,
    sink: std::io::Sink,
}

impl sot_log::capsule::producer::Producer for FakeProducer {
    type Output = FakeOutput;

    fn pre_spawn_detail() -> serde_json::Value {
        serde_json::json!({})
    }

    fn spawn(_argv: &[String], _cols: u16, _rows: u16) -> sot_log::Result<Self> {
        let script = FAKE_SCRIPT.lock().unwrap().take().expect("no fake script armed");
        let state = Arc::new(FakeState {
            exit_at: std::sync::Mutex::new(match script.exit {
                ExitObservable::AtSpawn => Some(Instant::now()),
                _ => None,
            }),
            exit_after_confirming: match script.exit {
                ExitObservable::AfterConfirmationStarts(d) => Some(d),
                _ => None,
            },
            confirming: AtomicBool::new(false),
            domain: script.domain,
        });
        let (steps_tx, steps_rx) = mpsc::channel();
        {
            let steps_tx = steps_tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(script.output_ends_after);
                let _ = steps_tx.send(script.output_end);
            });
        }
        Ok(FakeProducer {
            state,
            output: Some(FakeOutput { steps: steps_rx }),
            steps: Some(steps_tx),
            sink: std::io::sink(),
        })
    }

    fn take_output(&mut self) -> Self::Output {
        self.output.take().expect("take_output called twice")
    }

    fn input(&mut self) -> &mut dyn std::io::Write {
        &mut self.sink
    }

    fn resize(&self, _cols: u16, _rows: u16) -> sot_log::Result<()> {
        Ok(())
    }

    fn wait(&self, timeout: Duration) -> sot_log::Result<bool> {
        if timeout > Duration::ZERO {
            self.state.confirming.store(true, Ordering::Release);
            if let Some(d) = self.state.exit_after_confirming {
                self.state.exit_at.lock().unwrap().get_or_insert(Instant::now() + d);
            }
        }
        let deadline = Instant::now() + timeout;
        loop {
            if self.state.exited() {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn exit_status_after_confirmed_exit(&self) -> sot_log::Result<ExitStatus> {
        Ok(ExitStatus::Code(0))
    }

    fn terminate_domain(&self) -> sot_log::Result<()> {
        Ok(())
    }

    fn domain_is_empty(&self) -> sot_log::Result<bool> {
        Ok(self.state.exited()
            && match self.state.domain {
                Domain::EmptyOnceExited => true,
                Domain::EmptyOnceTheExitIsConfirmed => self.state.confirming.load(Ordering::Acquire),
            })
    }

    fn close_output_side(&mut self) -> std::thread::JoinHandle<()> {
        self.steps = None; // a blocked read now ends: the ordinary EOF
        std::thread::spawn(|| {})
    }
}

/// Arms the script and runs one capsule over the fake producer.
fn run_fake(
    dir: &std::path::Path,
    name: &str,
    script: FakeScript,
) -> (sot_log::Result<capsule::ExitSummary>, std::path::PathBuf) {
    *FAKE_SCRIPT.lock().unwrap() = Some(script);
    let cfg = config(dir, name, vec!["fake-producer".to_string()], 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    (capsule::run::<FakeProducer>(cfg, rx, &mut transport), root)
}

/// How many `producer_dead` frames the voyage actually sealed — the
/// question "did this run seal anything" wants, tolerant of a segment a
/// failed run left unsealed (which is not readable strictly and must not
/// panic the test that is asserting the absence).
fn sealed_producer_dead_count(root: &std::path::Path) -> usize {
    let seg_dir = root.join("seg");
    let Ok(entries) = std::fs::read_dir(&seg_dir) else {
        return 0;
    };
    let mut n = 0;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("sotseg") {
            continue;
        }
        let Ok(r) = SegmentReader::read(&p, true) else { continue };
        for f in r.frames {
            if let Some(payload) = f.payload.as_ref() {
                if payload["kind"] == "producer_dead" {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Ordering B of the amended decision 12 (the writer loop's own
/// `wait(ZERO)` observes the exit first, and the reader's terminal event
/// is still in the channel when teardown starts): teardown Phase A picks
/// the event up, confirms it against the exit it already knows about, and
/// RECORDS it. The run seals, verifies, and reports the same
/// `ProducerExited` it would have without the early end.
#[test]
fn early_output_end_after_the_exit_is_recorded_and_the_run_still_seals() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_b",
        FakeScript {
            output_ends_after: Duration::ZERO,
            output_end: OutputEnd::Eof,
            exit: ExitObservable::AtSpawn,
            // Not empty until the exit is confirmed after the terminal
            // event: Phase A keeps servicing the channel until that event
            // lands, however long the script and reader threads take to
            // queue it. See
            // `early_output_end_is_not_recorded_when_the_domain_empties_first`
            // for what a domain that is already empty does instead.
            domain: Domain::EmptyOnceTheExitIsConfirmed,
        },
    );
    let summary = summary.expect("an early output end the producer's exit explains must not be fatal");
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "earlyend_b").unwrap();
    let frames = sealed_frames(&root, "earlyend_b");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail["output_ended_early"].is_string(),
        "the admitted case must be RECORDED, not forgiven: {detail:?}"
    );
}

/// Ordering A (the reader wins: the terminal event arrives while the main
/// loop is blocked in `recv_timeout`, and the exit only becomes observable
/// once the capsule starts confirming it, well inside the grace). Same outcome as ordering B, same
/// `exit_kind`, same recorded field — which is the determinism claim of
/// the ruling's race analysis, as an assertion.
#[test]
fn early_output_end_confirmed_inside_the_grace_is_recorded_and_the_run_still_seals() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_a",
        FakeScript {
            output_ends_after: Duration::from_millis(150),
            output_end: OutputEnd::Eof,
            exit: ExitObservable::AfterConfirmationStarts(Duration::from_millis(300)),
            domain: Domain::EmptyOnceExited,
        },
    );
    let summary = summary.expect("an exit confirmed inside the grace must not be fatal");
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "earlyend_a").unwrap();
    let frames = sealed_frames(&root, "earlyend_a");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail["output_ended_early"].is_string(),
        "the admitted case must be RECORDED, not forgiven: {detail:?}"
    );
}

/// The rule still forbids what the old one forbade: a terminal reader
/// event with the producer STILL ALIVE past the grace is capsule-fatal and
/// seals NOTHING. Without this test the lane has not discharged the
/// invariant — it has only widened it. (This is the one test that waits
/// out the real `READER_END_EXIT_GRACE`.)
#[test]
fn early_output_end_with_the_producer_alive_past_the_grace_is_fatal() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_fatal",
        FakeScript {
            output_ends_after: Duration::from_millis(50),
            output_end: OutputEnd::Eof,
            exit: ExitObservable::Never,
            domain: Domain::EmptyOnceExited,
        },
    );
    let err = summary.expect_err("an unexplained early output end must stay capsule-fatal");
    let text = format!("{err}");
    assert!(text.contains("reader reached its terminal state"), "got: {text}");
    assert!(text.contains("still alive"), "the error must name the producer's liveness: {text}");
    assert_eq!(
        sealed_producer_dead_count(&root),
        0,
        "a fatal pre-close end of the output stream must seal nothing"
    );
}

/// Untouched by the amendment: a reader that ends WITHOUT a terminal event
/// at all (here, a panic inside `read`, which only its drop guard reports)
/// is still fatal on every platform, and the grace never enters into it.
#[test]
fn a_reader_gone_without_a_terminal_event_is_still_fatal() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "readergone",
        FakeScript {
            output_ends_after: Duration::from_millis(50),
            output_end: OutputEnd::PanicInRead,
            exit: ExitObservable::Never,
            domain: Domain::EmptyOnceExited,
        },
    );
    let err = summary.expect_err("a reader gone without a terminal Done must stay fatal");
    let text = format!("{err}");
    assert!(text.contains("without a terminal Done event"), "got: {text}");
    assert_eq!(sealed_producer_dead_count(&root), 0, "a reader-gone must seal nothing");
}

/// The residual the ruling's race analysis does not cover, pinned so it is
/// a known fact rather than folklore: in ordering B, if the containment
/// domain reports EMPTY on teardown's very first check, Phase A never
/// services the channel, and the terminal event that arrived before the
/// close is consumed by Phase B's drain — which cannot tell it apart from
/// the EOF its own close produces. The run is correct (the producer's exit
/// was confirmed by the main loop's own poll, which is the invariant) and
/// seals; it simply does not carry the recorded field. Recording in that
/// ordering is therefore best-effort, and this test says so out loud.
#[test]
fn early_output_end_is_not_recorded_when_the_domain_empties_first() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let (summary, root) = run_fake(
        dir.path(),
        "earlyend_fast",
        FakeScript {
            output_ends_after: Duration::ZERO,
            output_end: OutputEnd::Eof,
            exit: ExitObservable::AtSpawn,
            domain: Domain::EmptyOnceExited,
        },
    );
    let summary = summary.expect("this ordering is correct, just unrecorded");
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    verify_voyage(&root, "earlyend_fast").unwrap();
    let frames = sealed_frames(&root, "earlyend_fast");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail.get("output_ended_early").is_none(),
        "the drain cannot distinguish this event from its own close's EOF: {detail:?}"
    );
}

/// The isolation assertion the ruling requires, on the REAL producer this
/// platform ships: a Linux or Windows capsule's record never carries
/// `output_ended_early`, because the arm that sets it is unreachable there
/// (decision 12's held slave; ConPTY's `hOutput` outliving the child).
/// macOS is excluded deliberately — it is the one platform where a session
/// leader's exit revokes the pty under the reader and the field may
/// legitimately appear.
#[test]
#[cfg(not(target_os = "macos"))]
fn a_real_producer_never_records_an_early_output_end() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let argv = shell_command("exit 0");
    let cfg = config(dir.path(), "noearly1", argv, 80, 25);
    let root = cfg.voyage_root.clone();
    let (_tx, rx) = mpsc::channel();
    let mut transport = no_transport();
    let summary = capsule::run::<P>(cfg, rx, &mut transport).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    let frames = sealed_frames(&root, "noearly1");
    let detail = assert_producer_dead_is_last(&frames);
    assert!(
        detail.get("output_ended_early").is_none(),
        "this platform's output side cannot end before the loop closes it: {detail:?}"
    );
}

/// A probe for `config`'s voyage: true while another handle holds its
/// writer lock (`lock_writer` retries 250 ms, then fails closed).
fn writer_lock_held_probe(cfg: &CapsuleConfig) -> Arc<dyn Fn() -> bool + Send + Sync> {
    let lock = cfg.voyage_root.join("writer.lock");
    Arc::new(move || sot_log::lock_writer(&lock).is_err())
}

/// `ShutdownGuard`'s fallback `shutdown_all` runs while the voyage's writer
/// lock is still held, on every way `run` ends. Pins the drop order `Leg`
/// keeps: its transport field drops before its store field.
#[test]
fn shutdown_guard_runs_while_the_writer_lock_is_held() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();

    // 1. A natural exit: `run`'s own call, then the guard's.
    let cfg = config(dir.path(), "guard1", shell_command("exit 0"), 80, 25);
    let transport = TestTransport::new();
    transport.set_shutdown_probe(writer_lock_held_probe(&cfg));
    let (_tx, rx) = mpsc::channel();
    let summary = capsule::run::<P>(cfg, rx, &mut transport.clone()).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::ProducerExited);
    assert_eq!(transport.shutdown_probe_results(), vec![true, true]);

    // 2. An error after the loop (the aggregate teardown expires): both calls still precede the lock's release.
    let cfg = config(dir.path(), "guard2", shell_command("exit 0"), 80, 25);
    let transport = TestTransport::new();
    transport.force_shutdown_expiry();
    transport.set_shutdown_probe(writer_lock_held_probe(&cfg));
    let (_tx, rx) = mpsc::channel();
    capsule::run::<P>(cfg, rx, &mut transport.clone()).unwrap_err();
    assert_eq!(transport.shutdown_probe_results(), vec![true, true]);

    // 3. A spawn failure (geometry out of budget): only the guard's call.
    let cfg = config(dir.path(), "guard3", shell_command("exit 0"), 1, 25);
    let transport = TestTransport::new();
    transport.set_shutdown_probe(writer_lock_held_probe(&cfg));
    let (_tx, rx) = mpsc::channel();
    let summary = capsule::run::<P>(cfg, rx, &mut transport.clone()).unwrap();
    assert_eq!(summary.exit_kind, ExitKind::SpawnFailed);
    assert_eq!(transport.shutdown_probe_results(), vec![true]);

    // 4. An error inside the main loop (the output ended with the producer alive past the grace).
    *FAKE_SCRIPT.lock().unwrap() = Some(FakeScript {
        output_ends_after: Duration::from_millis(50),
        output_end: OutputEnd::Eof,
        exit: ExitObservable::Never,
        domain: Domain::EmptyOnceExited,
    });
    let cfg = config(dir.path(), "guard4", vec!["fake-producer".to_string()], 80, 25);
    let transport = TestTransport::new();
    transport.set_shutdown_probe(writer_lock_held_probe(&cfg));
    let (_tx, rx) = mpsc::channel();
    capsule::run::<FakeProducer>(cfg, rx, &mut transport.clone()).unwrap_err();
    assert_eq!(transport.shutdown_probe_results(), vec![true]);
}
