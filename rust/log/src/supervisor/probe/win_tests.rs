use super::*;

/// Spawns `cmd /c exit N` through `RealProbeOps::spawn` and waits,
/// bounded, for it to exit.
fn ended(code: u32) -> LegProcess {
    let mut command = std::process::Command::new("cmd.exe");
    command.args(["/d", "/c", &format!("exit {code}")]);
    let SpawnOutcome::Spawned(child) = RealProbeOps.spawn(&mut command) else {
        panic!("spawn exit {code}");
    };
    let leg = LegProcess::Owned(child);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !leg.wait(Duration::from_millis(100)).unwrap() {
        assert!(Instant::now() < deadline, "exit {code} did not end within 30 s");
    }
    leg
}

/// An owned leg keeps how it ended: the exit wait reads the code and
/// every later ask gives the same answer.
#[test]
fn an_owned_leg_keeps_its_exit_status() {
    for code in [71, 3] {
        let leg = ended(code);
        assert_eq!(leg.reap(), Some(ExitStatus::Code(code)), "exit {code}");
        assert_eq!(leg.reap(), Some(ExitStatus::Code(code)), "exit {code}: a second ask");
    }
}
