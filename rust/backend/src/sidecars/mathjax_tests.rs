use super::*;

/// A node child that fails its start leaves its own words in the log at warn: the stand-in for node is `sh` running
/// a script that writes node's error to stderr and exits before its ready line.
#[cfg(unix)]
#[tokio::test]
async fn a_mathjax_child_that_fails_its_start_logs_its_stderr_at_warn() {
    use crate::sidecars::contract_tests::within;
    use std::time::Duration;
    let log = sot_log::test_log::capture();
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("render.sh");
    std::fs::write(&script, "echo 'Error: Cannot find module (mathjax-boot-red)' >&2\nexit 1\n").unwrap();
    let sig: &'static sot_containment::Signal =
        Box::leak(Box::new(sot_containment::Signal::new()));
    let mathjax = MathJax {
        inner: Arc::new(MathJaxInner { script_path: script, node_bin: "/bin/sh".to_string(), submit: Mutex::new(None), sig }),
    };
    let err = tokio::time::timeout(Duration::from_secs(30), mathjax.render("x", false))
        .await
        .expect("the render is still waiting at 30 s")
        .expect_err("a child that exits before it is ready");
    assert!(format!("{err:#}").contains("before it was ready"), "{err:#}");
    within(Duration::from_secs(5), "the child's stderr is in the log at warn", || crate::sidecars::warned(&log.text(), "mathjax-boot-red")).await;
}

/// A render the child never answers is an error at `RENDER_BOUND` and ends that child, and the next render starts
/// a fresh one. The stand-in for node is `sh` running a script that says it is ready, then reads and never answers.
#[cfg(unix)]
#[tokio::test]
async fn a_render_past_its_bound_is_an_error_and_ends_its_child() {
    use crate::sidecars::contract_tests::within;
    use std::time::Duration;
    let dir = tempfile::Builder::new().prefix("sot-mathjax-bound-").tempdir().unwrap();
    let script = dir.path().join("render.sh");
    let starts = dir.path().join("starts");
    std::fs::write(
        &script,
        format!("echo started >> '{}'\necho '{{\"ready\":true}}'\nwhile read -r line; do :; done\n", starts.display()),
    )
    .unwrap();
    let sig: &'static sot_containment::Signal =
        Box::leak(Box::new(sot_containment::Signal::new()));
    let mathjax = MathJax {
        inner: Arc::new(MathJaxInner {
            script_path: script,
            node_bin: "/bin/sh".to_string(),
            submit: Mutex::new(None),
            sig,
        }),
    };
    let started_lines = || std::fs::read_to_string(&starts).map(|s| s.lines().count()).unwrap_or(0);

    let t0 = std::time::Instant::now();
    let err = tokio::time::timeout(Duration::from_secs(30), mathjax.render("x", false))
        .await
        .expect("the render is still waiting at 30 s")
        .expect_err("a render the child never answers");
    let took = t0.elapsed();
    let bound = Duration::from_secs(10); // RENDER_BOUND, written out so this test also runs on the unbounded code
    assert!(took >= bound && took < bound + Duration::from_secs(5), "ended after {took:?}");
    assert!(format!("{err:#}").contains("timed out after 10s"), "{err:#}");
    within(Duration::from_secs(5), "the timed-out child is ended", || sig.live() == 0).await;
    assert_eq!(started_lines(), 1);

    let next = {
        let mathjax = mathjax.clone();
        tokio::spawn(async move { mathjax.render("y", false).await })
    };
    within(Duration::from_secs(5), "the next render starts a fresh child", || {
        sig.live() == 1 && started_lines() == 2
    })
    .await;
    println!("mathjax-bound: {err:#} after {took:?}; the child ended; the next render started child {}", started_lines());
    next.abort();
    drop(mathjax);
    within(Duration::from_secs(5), "the fresh child ends with its handle", || sig.live() == 0).await;
}

/// A child that never says it is ready fails the render at `START_BOUND`, naming its start, not at `RENDER_BOUND`,
/// and is ended. The stand-in for node is `sh` running a script that reads and never answers.
#[cfg(unix)]
#[tokio::test]
async fn a_child_never_ready_is_ended_at_its_start_bound() {
    use crate::sidecars::contract_tests::within;
    use std::time::Duration;
    let dir = tempfile::Builder::new().prefix("sot-mathjax-start-").tempdir().unwrap();
    let script = dir.path().join("render.sh");
    std::fs::write(&script, "while read -r line; do :; done\n").unwrap();
    let sig: &'static sot_containment::Signal =
        Box::leak(Box::new(sot_containment::Signal::new()));
    let mathjax = MathJax {
        inner: Arc::new(MathJaxInner {
            script_path: script,
            node_bin: "/bin/sh".to_string(),
            submit: Mutex::new(None),
            sig,
        }),
    };
    let t0 = std::time::Instant::now();
    let err = tokio::time::timeout(Duration::from_secs(90), mathjax.render("x", false))
        .await
        .expect("the render is still waiting at 90 s")
        .expect_err("a child that never says it is ready");
    let took = t0.elapsed();
    println!("mathjax-start: {err:#} after {took:?}");
    let bound = Duration::from_secs(60); // START_BOUND, written out so this test also runs on the code without it
    assert!(format!("{err:#}").contains("was not ready within 60s"), "{err:#}");
    assert!(took >= bound && took < bound + Duration::from_secs(5), "ended after {took:?}");
    within(Duration::from_secs(5), "the child that never said ready is ended", || sig.live() == 0).await;
}

/// ADR 0049, User isolation: a MathJax line that does not parse is logged by position and length, never its bytes.
#[test]
fn a_mathjax_line_that_does_not_parse_is_logged_without_its_bytes() {
    let logged = crate::sidecars::logged_by(|| {
        let mut pending = HashMap::new();
        route_response(r#"{"id":2,"svg":"<svg data-u="http://127.0.0.1:41234/0123456789ab"#, &mut pending);
    });
    assert_eq!(logged.lines().filter(|l| l.contains("WARN")).count(), 1, "{logged}");
    assert!(!logged.contains("0123") && !logged.contains("41234"), "{logged}");
}
