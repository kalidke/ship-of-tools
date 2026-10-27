use super::*;

/// A listing that gave no answer presumes a daemon of the install runs, with its reason, so it is asked about and never
/// read as "none runs"; after a stage it stops nothing and the update stays armed, the run ending ONE STEP LEFT naming
/// why.
#[test]
fn a_listing_with_no_answer_is_presumed_running_and_leaves_the_update_armed() {
    let why = "`powershell.exe` gave no answer: powershell.exe did not finish within 60000 ms";
    assert_eq!(runs_in(Err(why.into()), Path::new(r"C:\p")), (true, Some(why.to_string())));
    let step = armed_listing(Err(why.into()), "v0.7.0").unwrap_err();
    let mut found = Found::default();
    found.step(step);
    let (line, code) = verdict::line(&found, "");
    assert!(code == 0 && line.starts_with("ONE STEP LEFT: ") && line.contains(why) && line.contains("stopped nothing"), "exit {code}: {line}");
}
