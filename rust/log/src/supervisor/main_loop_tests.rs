use super::*;
use std::os::fd::IntoRawFd;

/// A supervisor whose parent is gone before the takeover still becomes the authority: the acknowledgement cannot be
/// delivered (the channel has no reader), and the inherited claim stays this supervisor's fence. The claim is a
/// close-only fence held through an inheritable copy, so a fork by any other test of this binary could keep it held
/// after the drop below: the body runs in a process of its own.
#[test]
fn a_takeover_nobody_reads_keeps_the_claim() {
    if !sot_platform::test_isolated::run_isolated(
        "supervisor::main_loop::tests::a_takeover_nobody_reads_keeps_the_claim",
    ) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let claim = super::super::birth_claim::BirthClaim::take(dir.path())
        .expect("claim the fence for a birth");
    // SAFETY: a duplicate of a descriptor this test holds: the child's copy of the claim, as the launcher leaves it.
    let claim_fd = unsafe { libc::dup(claim.as_raw_fd()) };
    assert!(claim_fd >= 0, "dup the claim");
    drop(claim);
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let config = SuperviseConfig {
        state_dir: dir.path().to_path_buf(),
        mode: StartMode::Start,
        producer_argv: vec!["unused".into()],
        cols: 80,
        rows: 24,
        assume_no_rollback_target: true,
        survival: Survival::Normal,
        first_leg_only: Vec::new(),
        birth: Some(super::super::birth_claim::InheritedBirth {
            claim_fd,
            takeover_fd: writer.into_raw_fd(),
        }),
    };
    let fence = take_authority(&config).expect("the claim was lost with the parent's channel");
    assert!(matches!(fence, Fence::Inherited(_)));
    assert!(
        crate::supervisor::journal::fence::lock_supervisor(dir.path()).is_err(),
        "the fence was free while the authority held its claim"
    );
    drop(fence);
    assert!(
        crate::supervisor::journal::fence::lock_supervisor(dir.path()).is_ok(),
        "the fence stayed held after the authority let go"
    );
}
