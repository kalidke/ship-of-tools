use super::*;

/// A converge is applied only once the apply said the tag applied and the launch said DONE after it; a supervisor whose
/// start line names no code id gets no converge.
#[test]
fn a_converge_is_done_only_after_the_apply_and_the_launch_say_so() {
    assert_eq!(converge_state("", "v1"), Converge::Waiting);
    assert_eq!(converge_state("status: DONE\nsot-apply: APPLIED v1 (previous kept)\n", "v1"), Converge::Waiting);
    assert_eq!(converge_state("sot-apply: APPLIED v1 (previous kept)\nstatus: Starting\nstatus: DONE\n", "v1"), Converge::Done);
    assert_eq!(converge_state("sot-apply: APPLIED v0 (x)\nstatus: DONE\n", "v1"), Converge::Waiting);
    assert_eq!(converge_state("sot-apply: installing sotd failed - restoring previous binaries and pointers\n", "v1"), Converge::Restored);
    let log = "t  pid=7  supervisor start (relaunched=False) depth=0 ws=90MB code=AAA-BBB\nt  pid=9  supervisor start (relaunched=False) depth=0 ws=90MB code=CCC\nt  pid=7  supervisor start (relaunched=True) depth=1 ws=91MB code=DDD\n";
    assert_eq!(start_code(log, 7), "DDD");
    assert_eq!(start_code(log, 9), "CCC");
    assert_eq!(start_code("t  pid=7  supervisor start (relaunched=False) depth=0 ws=90MB code=unknown\n", 7), "");
    assert_eq!(start_code("t  pid=77  supervisor start code=X\n", 7), "");
}
