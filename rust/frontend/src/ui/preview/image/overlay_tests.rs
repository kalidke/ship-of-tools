use super::*;

#[test]
fn caption_store_is_keyed_by_workspace_and_evicts_fifo() {
    let mut s = CaptionStore::default();
    s.set(
        ("<host>".into(), "wsA".into()),
        "files:p.png".into(),
        "A's caption".into(),
    );
    s.set(
        ("<host>".into(), "wsB".into()),
        "files:p.png".into(),
        "B's caption".into(),
    );
    // Same FILE, different workspace → distinct entries. This is the ADR-0034
    // F2 hazard (one workspace's annotation over another's image) made
    // unrepresentable rather than merely avoided.
    assert_eq!(
        s.get(&("<host>".into(), "wsA".into()), "files:p.png")
            .unwrap(),
        "A's caption"
    );
    assert_eq!(
        s.get(&("<host>".into(), "wsB".into()), "files:p.png")
            .unwrap(),
        "B's caption"
    );
    assert_eq!(s.get(&("<host>".into(), "wsC".into()), "files:p.png"), None);
    // Latest-wins on re-set, and no duplicate order entry.
    s.set(
        ("<host>".into(), "wsA".into()),
        "files:p.png".into(),
        "A's second".into(),
    );
    assert_eq!(
        s.get(&("<host>".into(), "wsA".into()), "files:p.png")
            .unwrap(),
        "A's second"
    );
    assert_eq!(s.order.len(), 2);
    // Retiring one leaves the other alone.
    s.clear_one(&("<host>".into(), "wsA".into()), "files:p.png");
    assert_eq!(s.get(&("<host>".into(), "wsA".into()), "files:p.png"), None);
    assert_eq!(
        s.get(&("<host>".into(), "wsB".into()), "files:p.png")
            .unwrap(),
        "B's caption"
    );
    assert_eq!(s.order.len(), 1);
    // Bounded: past the cap the oldest badge is evicted, newest retained.
    let mut s = CaptionStore::default();
    for i in 0..(CAPTION_STORE_CAP + 10) {
        s.set(
            ("<host>".into(), "ws".into()),
            format!("files:{i}.png"),
            format!("cap {i}"),
        );
    }
    assert_eq!(s.map.len(), CAPTION_STORE_CAP);
    assert_eq!(
        s.get(&("<host>".into(), "ws".into()), "files:0.png"),
        None,
        "oldest evicted"
    );
    let newest = CAPTION_STORE_CAP + 9;
    assert_eq!(
        s.get(
            &("<host>".into(), "ws".into()),
            &format!("files:{newest}.png")
        )
        .unwrap(),
        &format!("cap {newest}")
    );
}

#[test]
fn snap_1_2_5_hits_nice_values() {
    assert_eq!(snap_1_2_5(1.0), 1.0);
    assert_eq!(snap_1_2_5(1.3), 1.0);
    assert_eq!(snap_1_2_5(1.7), 2.0);
    assert_eq!(snap_1_2_5(3.0), 2.0);
    assert_eq!(snap_1_2_5(4.0), 5.0);
    assert_eq!(snap_1_2_5(9.0), 10.0);
    assert_eq!(snap_1_2_5(430.0), 500.0);
    assert_eq!(snap_1_2_5(1800.0), 2000.0);
    assert_eq!(snap_1_2_5(0.03), 0.02);
    // Degenerate inputs never panic.
    assert_eq!(snap_1_2_5(0.0), 1.0);
    assert_eq!(snap_1_2_5(-5.0), 1.0);
    assert_eq!(snap_1_2_5(f64::NAN), 1.0);
}

#[test]
fn fmt_scale_value_trims_cleanly() {
    assert_eq!(fmt_scale_value(500.0), "500");
    assert_eq!(fmt_scale_value(2.0), "2");
    assert_eq!(fmt_scale_value(2.5), "2.5");
    assert_eq!(fmt_scale_value(0.02), "0.02");
}

#[test]
fn fmt_scale_label_promotes_nm_to_um_only_past_1000() {
    // The two live-verified fixtures must keep reading exactly as they do.
    assert_eq!(fmt_scale_label(200.0, "nm"), "200 nm");
    assert_eq!(fmt_scale_label(500.0, "nm"), "500 nm");
    // Boundary: 1000 nm promotes.
    assert_eq!(fmt_scale_label(999.0, "nm"), "999 nm");
    assert_eq!(fmt_scale_label(1000.0, "nm"), "1 µm");
    assert_eq!(fmt_scale_label(2000.0, "nm"), "2 µm");
    // A 0.1 µm/px camera frame lands here rather than "50000 nm".
    assert_eq!(fmt_scale_label(50000.0, "nm"), "50 µm");
    // Fractional promotion trims cleanly.
    assert_eq!(fmt_scale_label(2500.0, "nm"), "2.5 µm");
    // Non-nm units pass through untouched — we don't know their ladder.
    assert_eq!(fmt_scale_label(5000.0, "px"), "5000 px");
    assert_eq!(fmt_scale_label(2.0, "µm"), "2 µm");
}

#[test]
fn parse_nm_pixel_size_passes_through_and_rejects_junk() {
    // nm in, nm out — the entry unit IS the schema unit, so no conversion.
    // A typical SMLM pixel is ~10 nm, which is why nm beats µm for entry.
    assert_eq!(parse_nm_pixel_size("9.78"), Some(9.78));
    assert_eq!(parse_nm_pixel_size("100"), Some(100.0));
    assert_eq!(parse_nm_pixel_size("  65 "), Some(65.0));
    // A typo must not install a nonsense calibration.
    assert_eq!(parse_nm_pixel_size(""), None);
    assert_eq!(parse_nm_pixel_size("abc"), None);
    assert_eq!(parse_nm_pixel_size("0"), None);
    assert_eq!(parse_nm_pixel_size("-1"), None);
    assert_eq!(parse_nm_pixel_size("inf"), None);
}

#[test]
fn parse_physical_scale_reads_axes_and_unit() {
    let v = serde_json::json!({
        "physical_scale": {
            "axes": [
                {"name": "x", "nm_per_px": 2.0},
                {"name": "y", "nm_per_px": 2.0}
            ],
            "unit": "nm"
        }
    });
    let ps = parse_physical_scale(&v).expect("parses");
    assert_eq!(ps.unit, "nm");
    assert_eq!(ps.axes.len(), 2);
    assert_eq!(ps.axes[0].name, "x");
    assert_eq!(ps.axes[0].nm_per_px, 2.0);
}

#[test]
fn invalid_scale_is_not_displayable() {
    for ps in [
        serde_json::json!({"axes": [{"name": "x", "nm_per_px": 2.0}]}),
        serde_json::json!({"axes": [{"nm_per_px": 2.0}], "unit": "nm"}),
        serde_json::json!({"axes": [{"name": "x", "nm_per_px": 0.0}], "unit": "nm"}),
        serde_json::json!({"axes": [{"name": "x", "nm_per_px": -3.0}], "unit": "nm"}),
        serde_json::json!({"axes": [{"name": "x", "nm_per_px": "2"}], "unit": "nm"}),
    ] {
        assert_eq!(
            parse_physical_scale(&serde_json::json!({ "physical_scale": ps })),
            None,
            "{ps}"
        );
    }
    // Empty strings keep their existing acceptance.
    let ok = serde_json::json!({"physical_scale": {"axes": [{"name": "", "nm_per_px": 2.0}], "unit": ""}});
    assert!(parse_physical_scale(&ok).is_some());
}

#[test]
fn parse_physical_scale_rejects_missing_or_empty() {
    // No physical_scale key → None (so it clears like preview_page).
    assert_eq!(parse_physical_scale(&serde_json::json!({"page": 1})), None);
    // Empty axes → None.
    assert_eq!(
        parse_physical_scale(&serde_json::json!({
            "physical_scale": {"axes": [], "unit": "nm"}
        })),
        None
    );
    // Axis missing nm_per_px → None (the whole parse fails, no partial).
    assert_eq!(
        parse_physical_scale(&serde_json::json!({
            "physical_scale": {"axes": [{"name": "x"}], "unit": "nm"}
        })),
        None
    );
}
