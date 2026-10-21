use super::*;

/// Spawns `script` through `RealProbeOps::spawn` and waits, bounded, for
/// it to exit.
fn ended(script: &str) -> LegProcess {
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", script]);
    let SpawnOutcome::Spawned(child) = RealProbeOps.spawn(&mut command) else {
        panic!("spawn {script:?}");
    };
    let leg = LegProcess::Owned(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !leg.wait(Duration::from_millis(100)).unwrap() {
        assert!(Instant::now() < deadline, "{script:?} did not exit within 30 s");
    }
    leg
}

/// An owned leg keeps how it ended: the one reap reads the status and
/// every later ask gives the same answer.
#[test]
fn an_owned_leg_keeps_its_exit_status() {
    for (script, expected) in [
        ("exit 71", ExitStatus::Code(71)),
        ("exit 3", ExitStatus::Code(3)),
        ("kill -9 $$", ExitStatus::Signal(9)),
    ] {
        let leg = ended(script);
        assert_eq!(leg.reap(), Some(expected), "{script}");
        assert_eq!(leg.reap(), Some(expected), "{script}: a second ask");
    }
}
