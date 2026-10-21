use super::*;

#[test]
fn a_parent_that_cannot_be_opened_is_the_top() {
    assert_eq!(step(500, 3, true, None, 100), Step::Top);
}

#[test]
fn a_parent_created_after_its_child_is_the_top() {
    assert_eq!(step(500, 3, true, Some(101), 100), Step::Top);
}

#[test]
fn a_parent_created_with_or_before_its_child_is_the_next_step() {
    assert_eq!(step(500, 3, true, Some(100), 100), Step::Next(100));
    assert_eq!(step(500, 3, true, Some(7), 100), Step::Next(7));
}

#[test]
fn a_parent_missing_from_the_snapshot_and_the_system_pids_are_the_top() {
    assert_eq!(step(500, 3, false, Some(7), 100), Step::Top);
    assert_eq!(step(0, 3, true, Some(7), 100), Step::Top);
    assert_eq!(step(4, 3, true, Some(7), 100), Step::Top);
}

#[test]
fn only_the_64_line_cap_is_truncation() {
    assert_eq!(step(500, 63, true, Some(7), 100), Step::Next(7));
    assert_eq!(step(500, 64, true, Some(7), 100), Step::Truncated);
    // the cap is for a chain that goes on; a top at line 64 is a whole walk
    assert_eq!(step(500, 64, true, None, 100), Step::Top);
}

#[test]
fn a_tab_stays_raw_and_a_newline_and_a_return_each_have_their_own_byte() {
    assert_eq!(encode_command_line("a\tb"), "a\tb");
    assert_eq!(encode_command_line("a\nb"), "a\u{1c}b");
    assert_eq!(encode_command_line("a\rb"), "a\u{1b}b");
    assert_eq!(encode_command_line("a b"), "a b");
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn no_argument_starts_at_this_process_and_from_names_the_start() {
    assert_eq!(parse_from(&args(&[])), Ok(None));
    assert_eq!(parse_from(&args(&["--from", "4242"])), Ok(Some(4242)));
}

#[test]
fn any_other_arguments_are_a_usage_error() {
    for bad in [&["--from"][..], &["--from", "x"], &["--from", "-1"], &["--from", "1", "2"], &["4242"], &["--to", "1"]] {
        assert!(parse_from(&args(bad)).is_err(), "{bad:?} parsed");
    }
}

#[test]
fn a_line_is_the_pid_the_exe_and_the_command_line_whose_tabs_stay_raw() {
    assert_eq!(line(4242, "bash.exe", "bash.exe -c\tx"), "4242\tbash.exe\tbash.exe -c\tx");
    assert_eq!(line(7, "claude.exe", ""), "7\tclaude.exe\t");
}
