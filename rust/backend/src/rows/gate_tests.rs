use super::*;

#[test]
fn run_gate_table() {
    let reg = Workspaces::new();
    // Open: every id is admitted.
    assert!(reg.begin_start("ws-a-1").is_ok());
    assert!(reg.begin_start("ws-b-2").is_ok());

    // Settle returns true once a held permit drops.
    let permit = reg.begin_start("ws-b-2").expect("open gate");
    let dropper = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(permit);
    });
    assert!(reg.close_gate_and_settle(Instant::now() + std::time::Duration::from_secs(5)));
    dropper.join().unwrap();

    // Closing refuses every id.
    for id in ["ws-a-1", "ws-b-2", "ws-new-3"] {
        let refused = reg.begin_start(id).err().expect("a closing gate refuses every start");
        assert!(refused.contains(id), "{refused}");
        assert!(refused.contains("this computer's backend is shutting down"), "{refused}");
    }

    // Settle returns false at the deadline while a permit is held.
    let reg = Workspaces::new();
    let _held = reg.begin_start("ws-a-1").expect("open gate");
    let started = Instant::now();
    assert!(!reg.close_gate_and_settle(started + std::time::Duration::from_millis(200)));
    assert!(started.elapsed() >= std::time::Duration::from_millis(200));
}
