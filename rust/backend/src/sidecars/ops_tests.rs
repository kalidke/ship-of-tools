use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};
use sot_protocol::reply::HandlerOutput;

use super::*;
use crate::rows::Workspace;

const PLUTO_HEADER: &str = "### A Pluto.jl notebook ###\n# v0.20.0\n";

/// The one reply frame's payload; no blob.
fn payload(out: HandlerOutput, op: &str) -> Value {
    assert_eq!(out.len(), 1, "{op}: one frame");
    let (frame, blob) = &out[0];
    assert!(blob.is_none(), "{op}: no blob");
    assert_eq!(frame.op, op);
    frame.payload.clone()
}

/// A Pluto handle that must never start: its project and script are nowhere, and every test using it asserts
/// `server_up` stays false.
fn idle_pluto() -> Pluto {
    Pluto::new(PathBuf::new(), PathBuf::new())
}

fn registered(root: &std::path::Path) -> Workspaces {
    let workspaces = Workspaces::new();
    workspaces.insert(Workspace::from_label("nb", root.to_path_buf(), false, "none".into(), String::new(), String::new()));
    workspaces
}

#[test]
fn a_kernel_that_is_unavailable_answers_its_own_code_for_each_way_it_is() {
    for unavailable in [
        crate::sidecars::kernel::KernelUnavailable::Dead { reason: "exited with 1".into() },
        crate::sidecars::kernel::KernelUnavailable::Starting { waited: Duration::from_secs(10) },
        crate::sidecars::kernel::KernelUnavailable::NoAnswer { op: "project.scan".into(), waited: Duration::from_secs(10) },
    ] {
        let said = unavailable.to_string();
        let got = kernel_error_payload(&anyhow::Error::new(unavailable), "project.scan");
        assert_eq!(
            got,
            json!({ "error": format!("Julia kernel unavailable: {said}"), "code": "kernel_unavailable", "kernel_op": "project.scan" })
        );
    }
}

#[test]
fn a_kernel_request_that_fails_for_another_reason_is_not_called_unavailable() {
    let got = kernel_error_payload(&anyhow::anyhow!("unknown op: nope"), "nope");
    assert_eq!(got, json!({ "error": "unknown op: nope", "code": "kernel_request_failed", "kernel_op": "nope" }));
}

/// A notebook outside every registered row's root is refused before Pluto is asked: no server starts, though the file
/// is a Pluto notebook and exists.
#[tokio::test]
async fn pluto_open_outside_every_row_is_refused_and_starts_no_server() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let nb = elsewhere.path().join("nb.jl");
    std::fs::write(&nb, PLUTO_HEADER).unwrap();
    let pluto = idle_pluto();
    for workspaces in [registered(root.path()), Workspaces::new()] {
        let out = handle_pluto_open(7, json!({ "path": nb.to_string_lossy() }), &Session::new(), &pluto, &workspaces)
            .await
            .unwrap();
        let got = payload(out, op::PLUTO_OPEN);
        assert_eq!(got["code"], "outside_workspace", "{got}");
        assert!(!pluto.server_up().await);
    }
}

/// A file inside a row that does not open with Pluto's header is refused, and so is a header on a later line.
#[tokio::test]
async fn pluto_open_of_a_file_without_pluto_s_first_line_is_refused_and_starts_no_server() {
    let root = tempfile::tempdir().unwrap();
    let workspaces = registered(root.path());
    let pluto = idle_pluto();
    for (name, text) in [("plain.jl", "println(1)\n"), ("late.jl", "using Pkg\n### A Pluto.jl notebook ###\n")] {
        let file = root.path().join(name);
        std::fs::write(&file, text).unwrap();
        let out = handle_pluto_open(7, json!({ "path": file.to_string_lossy() }), &Session::new(), &pluto, &workspaces)
            .await
            .unwrap();
        let got = payload(out, op::PLUTO_OPEN);
        assert_eq!(got["code"], "not_pluto_flavored", "{name}: {got}");
        assert!(!pluto.server_up().await);
    }
}

/// A render that fails answers `mathjax_render_failed` with the reason and no blob, and bumps no revision.
#[tokio::test]
async fn a_failed_math_render_answers_its_code_and_no_blob() {
    let dir = tempfile::tempdir().unwrap();
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let mathjax = MathJax::new(dir.path().join("no-render.mjs"), sig);
    let out = handle_math_render(7, json!({ "latex": "x^2", "display": false }), &Session::new(), &mathjax)
        .await
        .unwrap();
    let got = payload(out, op::MATH_RENDER);
    assert_eq!(got["code"], "mathjax_render_failed", "{got}");
    assert!(got["error"].as_str().unwrap().contains("render script missing"), "{got}");
}

/// A render with the real MathJax child answers the SVG as a blob whose descriptor is its length and
/// `image/svg+xml`, with the `ex` height and the display flag the request gave, and the session's revision unchanged.
/// The case that would break it: a descriptor whose length is not the blob's, or a bumped revision.
#[tokio::test]
async fn a_math_render_answers_the_svg_as_a_blob_with_its_ex_height() {
    use crate::sidecars::contract_tests::{executable, isolated, require_mathjax_modules};
    if !isolated("sidecars::ops::tests::a_math_render_answers_the_svg_as_a_blob_with_its_ex_height", Duration::from_secs(150)) {
        return;
    }
    require_mathjax_modules();
    // The node binary is read at construction.
    std::env::set_var("SOT_NODE_BIN", executable("node"));
    let sig: &'static sot_containment::Signal = Box::leak(Box::new(sot_containment::Signal::new()));
    let mathjax = MathJax::new(MathJax::default_script_path(), sig);
    let session = Session::new();
    let (_, before) = session.snapshot().await;
    let out = tokio::time::timeout(
        Duration::from_secs(90),
        handle_math_render(7, json!({ "latex": "x^2", "display": true }), &session, &mathjax),
    )
    .await
    .expect("the render answers")
    .unwrap();
    assert_eq!(out.len(), 1);
    let (frame, blob) = &out[0];
    let blob = blob.as_ref().expect("the SVG rides as a blob");
    assert!(String::from_utf8_lossy(blob).contains("<svg"));
    assert_eq!(frame.payload["blob"], json!({ "len": blob.len(), "mime": "image/svg+xml" }), "{}", frame.payload);
    assert_eq!(frame.payload["display"], json!(true));
    assert!(frame.payload["ex"].as_f64().is_some_and(|ex| ex > 0.0), "{}", frame.payload);
    let (_, after) = session.snapshot().await;
    assert_eq!(before, after, "a render bumps no revision");
}
