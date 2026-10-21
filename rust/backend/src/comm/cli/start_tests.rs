use super::*;
use crate::comm::test_home::TestHome;

#[test]
fn a_session_start_joins_and_prints_one_bootstrap_line_and_the_rules() {
    let t = TestHome::new();
    let o = session_start(&[]);
    let h = crate::comm::identity::derive::tier(1, &t.root(), "box1").1;
    assert_eq!(o.code, 0);
    assert!(o.stdout.contains(&format!("Joined sot-comm as @{h} ")), "{}", o.stdout);
    assert!(o.stdout.contains(&format!("\nBOOTSTRAP-ARM handle={h} identity=ok WAKE: daemon\n")), "{}", o.stdout);
    assert!(o.stdout.contains("Work-state (nav row colour)") && o.stdout.contains("PERSISTENT REPL"));
    assert_eq!(session_start(&["x".into()]).code, 2);
}

/// A slot that names another project's identity is never joined over without a pin.
#[test]
fn a_session_start_refuses_a_slot_another_project_holds() {
    let t = TestHome::new();
    // The shared slot is last-writer-wins; a private one is a session's own.
    let private = t.home.join("self").join("mine.txt");
    crate::comm::identity::self_file::write(&private, "other-box1", "other", "/other").unwrap();
    std::env::set_var("SOT_COMM_SELF_FILE", &private);
    let o = session_start(&[]);
    assert!(o.stdout.starts_with("BOOTSTRAP-ARM handle=none identity=FAIL"), "{}", o.stdout);
    assert!(o.stderr.contains("REFUSED:"), "{}", o.stderr);
}
