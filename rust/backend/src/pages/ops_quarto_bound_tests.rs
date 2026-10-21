use super::*;
use quarto_shutdown_tests::JuliaPin;
use std::time::Duration;

fn stub(dir: &std::path::Path, body: &str) -> String {
    let p = dir.join("stub-quarto");
    sot_log::test_exec::write_executable(&p, format!("#!/bin/sh\n{body}\n"));
    p.to_string_lossy().into_owned()
}

fn private_signal() -> &'static crate::lifecycle::child_signal::Signal {
    Box::leak(Box::new(crate::lifecycle::child_signal::Signal::new()))
}

/// A render that never ends is an error naming its bound, and its tree is killed.
#[tokio::test]
async fn a_render_past_its_bound_is_killed_and_named() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(&dir.path().join("julia"), None);
    let sig = private_signal();
    let hung = stub(dir.path(), "exec sleep 600");
    let began = std::time::Instant::now();
    let ran = tokio::time::timeout(
        Duration::from_secs(10),
        run_quarto(&hung, dir.path(), std::ffi::OsStr::new("doc.qmd"), "a.html", false, Duration::from_secs(2), sig),
    )
    .await
    .map_err(|_| "run_quarto had not returned after 10s")
    .unwrap_or_else(|why| panic!("{why}"));
    let err = ran.expect_err("a render past its bound");
    println!("quarto-bound: {err} after {:?}", began.elapsed());
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(err.to_string(), "quarto render did not finish within 2s");
    assert!(sig.held_groups().is_empty(), "the killed render's tree is still held");
}

/// A render that never ends keeps what it wrote to stderr in its timeout answer.
#[tokio::test]
async fn a_render_past_its_bound_keeps_what_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(&dir.path().join("julia"), None);
    let sig = private_signal();
    let hung = stub(dir.path(), "echo '  Installing Julia 1.13.2+0.x64.linux.gnu' >&2\necho >&2\necho 'Starting engine julia' >&2\nexec sleep 600");
    let ran = run_quarto(&hung, dir.path(), std::ffi::OsStr::new("doc.qmd"), "a.html", false, Duration::from_secs(2), sig).await;
    let err = ran.expect_err("a render past its bound");
    println!("quarto-bound: {err}");
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    let want = "quarto render did not finish within 2s; its last stderr lines: Installing Julia 1.13.2+0.x64.linux.gnu / Starting engine julia";
    assert_eq!(err.to_string(), want);
    assert!(sig.held_groups().is_empty(), "the killed render's tree is still held");
}

/// Two renders of one document run one after the other: a stand-in that fails when it finds the other mid-render
/// is never overlapped.
#[tokio::test]
async fn two_renders_of_one_document_do_not_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let _pin = JuliaPin::new(&dir.path().join("julia"), None);
    let sig = private_signal();
    let racy = stub(
        dir.path(),
        "mkdir \"$PWD/.render-of-doc\" 2>/dev/null || { echo overlap >&2; exit 3; }\nsleep 1\nrmdir \"$PWD/.render-of-doc\"",
    );
    let name = std::ffi::OsStr::new("doc.qmd");
    let one = run_quarto(&racy, dir.path(), name, "a.html", true, QUARTO_RUN_BOUND, sig);
    let two = run_quarto(&racy, dir.path(), name, "b.html", true, QUARTO_RUN_BOUND, sig);
    let (a, b) = tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(one, two) })
        .await
        .expect("two renders of one document did not finish in 30 s");
    let codes = [a, b].map(|r| r.ok().flatten().and_then(|o| o.status.code()));
    println!("quarto-bound: exit codes {codes:?}");
    assert_eq!(codes, [Some(0), Some(0)], "the renders overlapped");
    assert!(!RENDERING.lock().unwrap().contains_key(&dir.path().join("doc.qmd")), "a finished document stays in the render table");
}

/// The request id is per connection, so the output name is not built from it.
#[test]
fn each_render_has_its_own_output_name() {
    let (a, b) = (quarto_out_name(), quarto_out_name());
    assert_ne!(a, b);
    assert!(a.starts_with(&format!("__sot-qmd-{}-", std::process::id())) && a.ends_with(".html"), "{a}");
}
