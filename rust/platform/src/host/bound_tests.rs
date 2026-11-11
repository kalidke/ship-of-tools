use super::*;
use crate::test_isolated::{enter, test_command, wait_within, ISOLATION_TIMEOUT};
use std::process::{ExitStatus, Stdio};

/// The code every bound ends its child with here.
pub(crate) const CODE: i32 = 87;
const ROLE: &str = "SOT_TEST_BOUND_ROLE";

/// Set, to its role, in a child `run_child` started: the test's body then does the child's part.
pub(crate) fn child_role() -> Option<String> {
    std::env::var(ROLE).ok()
}

/// Re-runs `test` (its exact libtest name) as a child for `role`, which must call `enter(test)` first, and returns how
/// it ended with what it wrote to stdout. A body that ends its own process by a bound is not `run_isolated`'s
/// (that wants exit 0); the entry record is the same one.
pub(crate) fn run_child(test: &str, role: &str) -> (ExitStatus, String) {
    let (mut command, entry) = test_command(test);
    let mut child = command
        .env(ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the child");
    let status = wait_within(&mut child, ISOLATION_TIMEOUT);
    entry.assert_once(child.id());
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut out).unwrap();
    (status, out)
}

#[test]
fn exit_now_ends_with_its_code() {
    const NAME: &str = "host::bound::tests::exit_now_ends_with_its_code";
    if child_role().is_some() {
        enter(NAME);
        exit_now(CODE);
    }
    assert_eq!(run_child(NAME, "exit").0.code(), Some(CODE));
}
