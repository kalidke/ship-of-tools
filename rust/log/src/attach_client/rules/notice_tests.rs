use super::*;

// ---- (e) attach notice ------------------------------------------------

#[test]
fn attach_notice_text_is_the_pinned_wording() {
    assert_eq!(attach_notice_text("2026-09-01T12:00:00Z"), "attached to leg started 2026-09-01T12:00:00Z");
}

// ---- (f) fe_down --------------------------------------------------

#[test]
fn last_evidence_ts_reads_the_last_lines_ts() {
    let contents = "{\"ts\":\"t-old\"}\n{\"ts\":\"t-new\"}\n";
    assert_eq!(last_evidence_ts(contents).as_deref(), Some("t-new"));
}

#[test]
fn last_evidence_ts_none_on_empty_or_malformed_trailing_line() {
    assert_eq!(last_evidence_ts(""), None);
    assert_eq!(last_evidence_ts("not json\n"), None);
    assert_eq!(last_evidence_ts("{\"ts\":\"t-old\"}\nnot json\n"), None);
}

#[test]
fn marker_shape_matches_the_pinned_json() {
    let v = build_fe_down_marker("backend-dev", "t0", "t1");
    assert_eq!(v["from"], "sot-fe");
    assert_eq!(v["to"], "backend-dev");
    assert_eq!(v["kind"], "fe_down");
    assert_eq!(v["ts"], "t1");
    assert_eq!(v["window"]["last_evidence"], "t0");
    assert_eq!(v["text"], "possible relay gap: last inbox evidence t0, frontend reattached t1");
}

#[test]
fn no_baseline_no_marker() {
    let mut b = FeDownBaseline::capture(None);
    // Even on a SECOND attach (so "first attach" is not why), no
    // baseline still means no marker.
    b.marker_for_attach("h", "t1");
    assert!(b.marker_for_attach("h", "t2").is_none());
}

#[test]
fn first_attach_is_skipped_even_with_a_baseline() {
    let mut b = FeDownBaseline::capture(Some("t0".to_string()));
    assert!(b.marker_for_attach("h", "t1").is_none());
    let second = b.marker_for_attach("h", "t2").expect("second attach should mark");
    assert_eq!(second["window"]["last_evidence"], "t0");
    assert_eq!(second["ts"], "t2");
}
