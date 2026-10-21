/// `State` needs a window, so the wheel's presence hook is checked in the source: a wheel step reports a person
/// to the window in use, as a press does.
#[test]
fn a_wheel_step_reports_presence() {
    let src = include_str!("mouse.rs");
    let body = src.split("fn mouse_wheel(").nth(1).expect("mouse_wheel");
    let body = body.split("\n}\n").next().unwrap_or(body);
    assert!(body.contains("state.report_presence(true)"), "the wheel does not count as input");
}
