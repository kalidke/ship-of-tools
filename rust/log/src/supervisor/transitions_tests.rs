use super::*;
use crate::supervisor::probe::ProbeOps;

struct Fixture {
    _dir: tempfile::TempDir,
    config: SuperviseConfig,
    lease: LegLease,
    authority: AuthorityState,
    capsule_exe: PathBuf,
}

/// The lease is a named mutex on Windows (named by the fixture and the process
/// id), so tests running in parallel each need a name of their own.
static NEXT_FIXTURE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().to_path_buf();
    let config = SuperviseConfig {
        state_dir: state_dir.clone(),
        mode: StartMode::Start,
        producer_argv: vec!["unused".into()],
        cols: 80,
        rows: 24,
        assume_no_rollback_target: true,
        survival: Survival::Normal,
        first_leg_only: Vec::new(),
        #[cfg(unix)]
        birth: None,
    };
    let authority = AuthorityState {
        state_dir,
        voyage_id: Some(uuid::Uuid::now_v7().to_string()),
        self_pid: 0,
        self_created: 0,
        stop_requested: None,
        producer_ran: false,
        retired_legs: Vec::new(),
    };
    Fixture {
        _dir: dir,
        config,
        lease: LegLease::create(&format!("transitions-test-{}", NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::SeqCst))).unwrap(),
        authority,
        capsule_exe: PathBuf::from("unused"),
    }
}

/// An owned leg that has already exited, as the lifecycle holds one.
fn exited_leg() -> LegProcess {
    #[cfg(unix)]
    let mut command = std::process::Command::new("/bin/sh");
    #[cfg(unix)]
    command.args(["-c", "exit 0"]);
    #[cfg(windows)]
    let mut command = std::process::Command::new("cmd.exe");
    #[cfg(windows)]
    command.args(["/d", "/c", "exit 0"]);
    let crate::supervisor::probe::SpawnOutcome::Spawned(child) = RealProbeOps.spawn(&mut command) else {
        panic!("spawn a short-lived leg");
    };
    LegProcess::Owned(child)
}

fn spawning(outcome: ProbeOutcome<LegProcess>) -> Lifecycle {
    let (tx, rx) = mpsc::channel();
    tx.send(outcome).unwrap();
    Lifecycle::Spawning { rx, handle: std::thread::spawn(|| {}), started_at: Instant::now() }
}

fn advance_one_spawning(f: &mut Fixture, counter: &mut u32, outcome: ProbeOutcome<LegProcess>) -> Lifecycle {
    let Lifecycle::Spawning { rx, handle, started_at } = spawning(outcome) else { unreachable!() };
    advance_spawning(rx, handle, started_at, counter, &f.capsule_exe, &f.config, &f.lease, &mut f.authority, Instant::now())
}

/// A leg that exits 71 is a storage exit: the crash counter is left
/// alone and the authority holds. The same leg with any other code is
/// counted, and the third unstable leg is Terminal.
#[test]
fn a_storage_leg_exit_keeps_the_crash_counter() {
    let mut f = fixture();
    let mut counter = 2;
    let held = advance_one_spawning(&mut f, &mut counter, ProbeOutcome::LegEnded(Some(ExitStatus::Code(71))));
    assert!(!matches!(held, Lifecycle::Terminal { .. }), "a storage exit never goes Terminal");
    assert!(matches!(held, Lifecycle::StorageFull(_)), "a storage exit holds the authority");
    assert_eq!(counter, 2, "a storage exit leaves the counter alone");

    let mut counter = 2;
    let counted = advance_one_spawning(&mut f, &mut counter, ProbeOutcome::LegEnded(Some(ExitStatus::Code(1))));
    assert!(matches!(counted, Lifecycle::Terminal { .. }), "an ordinary exit is counted: the third is Terminal");
    assert_eq!(counter, 3);
}

/// A storage failure of any authority worker holds the authority and
/// re-runs startup recovery once storage clears; nothing goes Terminal.
#[test]
fn authority_storage_failures_wait() {
    let mut f = fixture();
    let now = Instant::now();
    let expect = |lifecycle: Lifecycle, resume: storage::Resume| match lifecycle {
        Lifecycle::StorageFull(wait) => assert_eq!(wait.resume(), resume),
        _ => panic!("expected the storage wait"),
    };

    // Recovery.
    let (tx, rx) = mpsc::channel();
    tx.send(RecoveryOutcome::Storage("full".into())).unwrap();
    let next = advance_recovering(rx, std::thread::spawn(|| {}), now, &f.config, &mut f.authority, now);
    expect(next, storage::Resume::Recover);

    // An end_run worker.
    let (mut counter, mut step) = (0, 0);
    let next = end_run_finished(
        EndRunWorkerResult::Storage("full".into()),
        "op-1",
        exited_leg(),
        &mut counter,
        &mut step,
        &f.capsule_exe.clone(),
        &f.config,
        &f.lease,
        &mut f.authority,
        now,
    );
    expect(next, storage::Resume::Recover);
    assert_eq!(counter, 0, "an end_run storage failure does not touch the counter");

    // A reset worker.
    let (tx, rx) = mpsc::channel();
    tx.send(ResetWorkerResult::Storage("full".into())).unwrap();
    let next = advance_resetting("op-2".into(), rx, std::thread::spawn(|| {}), now, &f.capsule_exe.clone(), &f.config, &f.lease, &mut f.authority, now);
    expect(next, storage::Resume::Recover);
}

/// The first-leg-only tokens are kept while no producer has run in this
/// process and stripped from every leg after; a leg that reaches Ready
/// is the producer having run.
#[test]
fn first_leg_only_tokens_go_once_a_producer_has_run() {
    let mut f = fixture();
    f.config.producer_argv = vec!["agent".into(), "--resume".into(), "abc".into()];
    f.config.first_leg_only = vec!["--resume".into(), "abc".into()];
    let whole = f.config.producer_argv.clone();
    assert_eq!(leg_argv(&f.config, false), whole, "no producer ran: kept");
    assert_eq!(leg_argv(&f.config, true), vec!["agent".to_string()], "a producer ran: stripped");

    // A leg that reaches Ready sets `producer_ran`.
    assert!(!f.authority.producer_ran);
    let mut counter = 0;
    let ready = advance_one_spawning(&mut f, &mut counter, ProbeOutcome::Ready(exited_leg()));
    assert!(matches!(ready, Lifecycle::Ready { .. }));
    assert!(f.authority.producer_ran);
}

/// One tick of the supervisor lane against `lifecycle`, at `now`.
fn tick(
    lane: &Lane,
    conns: &mut HashMap<ConnId, Conn>,
    authority: &mut AuthorityState,
    lifecycle: &mut Lifecycle,
    now: Instant,
) {
    let mut ctx = LaneCtx {
        authority,
        lifecycle,
    };
    assert!(
        !service_lane(lane, None, conns, &mut ctx, now),
        "the accept loop is alive"
    );
}

/// Ticks at `now` until the lane has registered one more connection, and returns its id.
fn accept_one(
    lane: &Lane,
    conns: &mut HashMap<ConnId, Conn>,
    f: &mut Fixture,
    now: Instant,
) -> ConnId {
    let before: Vec<ConnId> = conns.keys().copied().collect();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        tick(
            lane,
            conns,
            &mut f.authority,
            &mut Lifecycle::EndedNoRespawn,
            now,
        );
        if let Some(id) = conns.keys().find(|id| !before.contains(id)) {
            return *id;
        }
        assert!(
            Instant::now() < deadline,
            "the lane registers the connection"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Two silent clients on the supervisor lane, both accepted at `t0`: `owed` is the one the test makes owed the
/// reply of an EndRun in flight, `other` an ordinary idle one. Both clients stay connected while this lives.
struct SilentClients {
    lane: Lane,
    conns: HashMap<ConnId, Conn>,
    owed: ConnId,
    other: ConnId,
    owed_client: OwedClient,
    _other_client: OwedClient,
}

/// The client type of the platform's supervisor lane endpoint.
type OwedClient = <PlatformEndpoint as Endpoint>::Client;

/// Binds the supervisor lane and accepts the two silent clients of [`SilentClients`].
fn silent_clients(f: &mut Fixture, t0: Instant) -> SilentClients {
    let h = crate::host::state_dir::state_dir_hash(&f.config.state_dir);
    let lane = Lane::bind_supervisor(&h, MAX_LANE_INSTANCES).expect("bind the supervisor lane");
    let endpoint = PlatformEndpoint::default();
    let mut conns = HashMap::new();
    let owed_client = endpoint
        .connect_supervisor_unchallenged(&h)
        .expect("connect the owed client");
    let owed = accept_one(&lane, &mut conns, f, t0);
    let _other_client = endpoint
        .connect_supervisor_unchallenged(&h)
        .expect("connect another client");
    let other = accept_one(&lane, &mut conns, f, t0);
    SilentClients {
        lane,
        conns,
        owed,
        other,
        owed_client,
        _other_client,
    }
}

/// An end_run in flight whose deferred reply is owed to `owed`; its worker reports on `rx`.
fn ending_owing(
    owed: ConnId,
    rx: mpsc::Receiver<EndingProgress>,
    started_at: Instant,
) -> Lifecycle {
    Lifecycle::Ending {
        operation_id: "op".into(),
        rx,
        handle: std::thread::spawn(|| {}),
        started_at,
        pending_reply: Some(owed),
        process: exited_leg(),
    }
}

/// Advances the end_run `lifecycle` once, after its worker has reported `RecordClosed`, as the lane's tick does.
fn deliver_record_closed(
    lifecycle: Lifecycle,
    lane: &Lane,
    conns: &mut HashMap<ConnId, Conn>,
    f: &mut Fixture,
    now: Instant,
) -> Lifecycle {
    let Lifecycle::Ending {
        operation_id,
        rx,
        handle,
        started_at,
        pending_reply,
        process,
    } = lifecycle
    else {
        unreachable!()
    };
    let (mut counter, mut step) = (0, 0);
    advance_ending(
        operation_id,
        rx,
        handle,
        started_at,
        pending_reply,
        process,
        lane,
        conns,
        &mut counter,
        &mut step,
        &f.capsule_exe,
        &f.config,
        &f.lease,
        &mut f.authority,
        now,
    )
}

/// The idle rule while an end_run is in flight, on a real lane with clock readings chosen by the test: the
/// connection owed the deferred reply is not closed as idle while every other silent one is; queuing the reply
/// restarts its idle clock, so the next tick leaves it to the client; after that the 5 s rule applies to it again.
#[test]
fn the_connection_owed_the_end_run_reply_is_not_idle_until_its_reply_and_then_gets_the_whole_deadline(
) {
    let test = "supervisor::transitions::tests::the_connection_owed_the_end_run_reply_is_not_idle_until_its_reply_and_then_gets_the_whole_deadline";
    if !crate::test_isolated::run_isolated(test) {
        return;
    }
    #[cfg(unix)]
    let _runtime = {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::Builder::new()
            .prefix("sot-idle-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::env::set_var("SOT_RUNTIME_DIR", root.path());
        root
    };
    let mut f = fixture();
    let t0 = Instant::now();
    let SilentClients {
        lane,
        mut conns,
        owed,
        other,
        owed_client,
        _other_client,
    } = silent_clients(&mut f, t0);

    let (tx, rx) = mpsc::channel();
    let mut lifecycle = ending_owing(owed, rx, t0);
    let replied = t0 + LANE_IDLE_DEADLINE + Duration::from_secs(1);
    tick(&lane, &mut conns, &mut f.authority, &mut lifecycle, replied);
    assert!(
        conns.contains_key(&owed),
        "the connection owed the reply is not idle"
    );
    assert!(
        !conns.contains_key(&other),
        "every other silent connection is closed at the deadline"
    );

    tx.send(EndingProgress::RecordClosed).unwrap();
    let mut lifecycle = deliver_record_closed(lifecycle, &lane, &mut conns, &mut f, replied);
    assert!(
        matches!(
            lifecycle,
            Lifecycle::Ending {
                pending_reply: None,
                ..
            }
        ),
        "the reply is no longer owed"
    );
    tick(
        &lane,
        &mut conns,
        &mut f.authority,
        &mut lifecycle,
        replied + MAIN_LOOP_POLL,
    );
    assert!(
        conns.contains_key(&owed),
        "the tick after the reply leaves the connection to its client"
    );

    let reply = crate::attach_client::supervisor_client::read_one_frame(
        &owed_client,
        Instant::now() + Duration::from_secs(5),
    );
    assert!(
        matches!(
            reply,
            Ok(DecodedFrame::SupervisorReply(SupervisorReply::Operation(
                SupervisorOperationState::RecordClosed
            )))
        ),
        "the owed client reads record_closed: {reply:?}"
    );

    tick(
        &lane,
        &mut conns,
        &mut f.authority,
        &mut lifecycle,
        replied + LANE_IDLE_DEADLINE,
    );
    assert!(
        !conns.contains_key(&owed),
        "once the reply is out, the idle deadline applies to it again"
    );
}
