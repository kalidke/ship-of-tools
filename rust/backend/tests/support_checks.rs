#![cfg(target_os = "linux")]
//! The shared support's own answers: a host check it makes fails with its tool's account when the tool cannot answer,
//! and never reads that failure as an answer.

#[allow(dead_code, reason = "the shared fixture serves more suites than this one uses")]
mod support;

/// `pgrep` exits 2 on a pattern it cannot compile: the checks built on it fail with pgrep's account instead of reading
/// "no process matches" or "none counted".
#[test]
fn a_pgrep_that_cannot_answer_fails_the_check_instead_of_answering_no() {
    let payload = std::panic::catch_unwind(|| support::any_process_matches("("))
        .expect_err("a pgrep that cannot answer read as an answer");
    let text = payload.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(text.contains("pgrep -f (") && text.contains("exit status: 2"), "{text}");
    let error = support::count_matching_processes("(").expect_err("a pgrep that cannot answer counted processes");
    assert!(error.to_string().contains("pgrep -f (") && error.to_string().contains("exit status: 2"), "{error}");
}
