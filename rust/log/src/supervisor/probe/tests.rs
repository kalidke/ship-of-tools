use super::*;

#[test]
fn scripted_connect_outcomes_drain_in_order_without_touching_the_os() {
    let ops = ScriptedProbeOps::new();
    ops.push_connect(ConnectOutcome::FileNotFound);
    ops.push_connect(ConnectOutcome::PipeBusy);
    assert!(matches!(ops.connect("unused"), ConnectOutcome::FileNotFound));
    assert!(matches!(ops.connect("unused"), ConnectOutcome::PipeBusy));
}

#[test]
fn scripted_wait_and_terminate_outcomes_drain_in_order_with_no_real_process() {
    let ops = ScriptedProbeOps::new();
    ops.push_wait_exit(WaitOutcome::StillRunning);
    ops.push_wait_exit(WaitOutcome::Exited);
    ops.push_terminate(Ok(()));
    ops.push_terminate(Err(std::io::Error::other("boom")));

    // A trivial zero-sized DummyProcess -- no pipe, no challenge, no
    // real Windows handle anywhere.
    let process = DummyProcess;
    assert_eq!(ops.wait_exit(&process, Duration::from_millis(1)), WaitOutcome::StillRunning);
    assert_eq!(ops.wait_exit(&process, Duration::from_millis(1)), WaitOutcome::Exited);
    assert!(ops.terminate(&process).is_ok());
    assert!(ops.terminate(&process).is_err());
}

#[test]
#[should_panic(expected = "exhausted")]
fn scripted_ops_panics_loudly_past_its_script() {
    let ops = ScriptedProbeOps::new();
    let _ = ops.connect("unused");
}

#[test]
fn injected_clock_advances_only_when_told() {
    let ops = ScriptedProbeOps::new();
    let t0 = ops.now();
    assert_eq!(ops.now(), t0, "the clock must never move on its own");
    ops.advance(Duration::from_secs(120));
    assert_eq!(ops.now(), t0 + Duration::from_secs(120));
}

/// The model-test scaffolding proof: every A1-A5/B0-B9 row is drivable through `ScriptedProbeOps`
/// alone, with NO real pipe, process, or challenge anywhere, and
/// every queue is exhausted by the end — proving the seam is total
/// over the classifier's own transition table without shipping any
/// of that table's decision logic here.
#[test]
fn every_probe_table_row_is_drivable_scripted_only() {
    let ops = ScriptedProbeOps::new();
    let unused_cmd = || std::process::Command::new("unused");

    // A1: CreateProcess failed -> SPAWN-FAILED.
    ops.push_spawn(SpawnOutcome::Failed(std::io::Error::other("spawn failed")));
    assert!(matches!(ops.spawn(&mut unused_cmd()), SpawnOutcome::Failed(_)));

    // A2: the child has exited -> LEG ENDED.
    ops.push_spawn(SpawnOutcome::Spawned(DummySpawnedChild));
    let SpawnOutcome::Spawned(child) = ops.spawn(&mut unused_cmd()) else { panic!("expected Spawned") };
    ops.push_wait_child(WaitOutcome::Exited);
    assert_eq!(ops.wait_child(&child, Duration::from_secs(1)), WaitOutcome::Exited);

    // A3: WAIT_FAILED, or the readiness cutoff expired -> KILL+WAIT.
    // The wait-failure half:
    ops.push_wait_child(WaitOutcome::WaitFailed);
    assert_eq!(ops.wait_child(&child, Duration::from_secs(1)), WaitOutcome::WaitFailed);
    // The readiness-cutoff half, via the injected clock -- no sleep:
    let readiness_cutoff = ops.now() + Duration::from_secs(60);
    ops.advance(Duration::from_secs(61));
    assert!(ops.now() >= readiness_cutoff, "the injected clock must be able to reach a blown cutoff");
    // Either half ends in KILL, THEN the post-kill WAIT -- the row
    // is named KILL+WAIT, not KILL alone.
    ops.push_kill_child(Ok(()));
    assert!(ops.kill_child(&child).is_ok());
    ops.push_wait_child(WaitOutcome::Exited);
    assert_eq!(ops.wait_child(&child, Duration::from_secs(1)), WaitOutcome::Exited);

    // A4: alive, within cutoff -> challenge on its pipe -> well-formed
    // status_ok, identity matches -> READY (shares `connect`/
    // `challenge` with Stage B by design).
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    assert!(matches!(
        ops.challenge(&conn, ops.now() + Duration::from_secs(2)),
        ChallengeOutcome::Proven(_)
    ));

    // A5: alive, within cutoff -> any OTHER challenge/connect outcome
    // -> PENDING.
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
    ops.push_challenge(ChallengeOutcome::Undetermined);
    assert!(matches!(
        ops.challenge(&conn, ops.now() + Duration::from_secs(2)),
        ChallengeOutcome::Undetermined
    ));

    // B0: episode deadline expired -> WEDGED. Purely the injected
    // clock again -- no real wall-clock wait.
    let episode_deadline = ops.now() + Duration::from_secs(60);
    ops.advance(Duration::from_secs(61));
    assert!(ops.now() >= episode_deadline);

    // B1: connect ok -> well-formed status_ok, identity matches ->
    // ADOPTED.
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
    ops.push_challenge(ChallengeOutcome::Proven(DummyProcess));
    assert!(matches!(ops.challenge(&conn, ops.now()), ChallengeOutcome::Proven(_)));

    // B2/B3/B4: connect ok -> SID differs / wrong opcode / undecodable
    // -> all fold to Foreign at this seam's level of observation (the
    // WHY is the challenge's own job, already covered by
    // challenge.rs's own frame tests).
    for _ in 0..3 {
        ops.push_connect(ConnectOutcome::Connected(DummyConn));
        let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
        ops.push_challenge(ChallengeOutcome::Foreign);
        assert!(matches!(ops.challenge(&conn, ops.now()), ChallengeOutcome::Foreign));
    }

    // B5: connect ok -> EOF/timeout/read-write error, or an
    // authentication OS-call failure -> PENDING.
    ops.push_connect(ConnectOutcome::Connected(DummyConn));
    let ConnectOutcome::Connected(conn) = ops.connect("voy") else { panic!("expected Connected") };
    ops.push_challenge(ChallengeOutcome::Undetermined);
    assert!(matches!(ops.challenge(&conn, ops.now()), ChallengeOutcome::Undetermined));

    // B6: connect ERROR_ACCESS_DENIED -> FOREIGN.
    ops.push_connect(ConnectOutcome::AccessDenied);
    assert!(matches!(ops.connect("voy"), ConnectOutcome::AccessDenied));

    // B7: connect ERROR_FILE_NOT_FOUND -> writer fence FREE -> ABSENT.
    ops.push_connect(ConnectOutcome::FileNotFound);
    assert!(matches!(ops.connect("voy"), ConnectOutcome::FileNotFound));
    ops.push_writer_fence_probe(FenceProbe::Free);
    assert!(matches!(ops.writer_fence_probe(Path::new("voy")), FenceProbe::Free));

    // B8: connect ERROR_FILE_NOT_FOUND -> fence held, or probing it
    // errored -> PENDING.
    ops.push_connect(ConnectOutcome::FileNotFound);
    assert!(matches!(ops.connect("voy"), ConnectOutcome::FileNotFound));
    ops.push_writer_fence_probe(FenceProbe::Held);
    assert!(matches!(ops.writer_fence_probe(Path::new("voy")), FenceProbe::Held));
    ops.push_connect(ConnectOutcome::FileNotFound);
    assert!(matches!(ops.connect("voy"), ConnectOutcome::FileNotFound));
    ops.push_writer_fence_probe(FenceProbe::Error(std::io::Error::other("probe failed")));
    assert!(matches!(ops.writer_fence_probe(Path::new("voy")), FenceProbe::Error(_)));

    // B9: connect ERROR_PIPE_BUSY, or any other connect error ->
    // PENDING.
    ops.push_connect(ConnectOutcome::PipeBusy);
    assert!(matches!(ops.connect("voy"), ConnectOutcome::PipeBusy));
    ops.push_connect(ConnectOutcome::OtherIo(std::io::Error::other("other")));
    assert!(matches!(ops.connect("voy"), ConnectOutcome::OtherIo(_)));

    assert!(ops.all_exhausted(), "every scripted queue must be fully drained");
}
