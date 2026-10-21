use super::*;
use serde_json::json;

#[test]
fn a_valid_scale_keeps_every_axis_in_order() {
    let ps = parse_physical_scale(&json!({
        "axes": [{"name": "x", "nm_per_px": 5.0}, {"name": "z", "nm_per_px": 20.0}],
        "unit": "nm"
    }))
    .unwrap();
    assert_eq!(ps.unit, "nm");
    assert_eq!(ps.axes.len(), 2);
    assert_eq!((ps.axes[0].name.as_str(), ps.axes[0].nm_per_px), ("x", 5.0));
    assert_eq!((ps.axes[1].name.as_str(), ps.axes[1].nm_per_px), ("z", 20.0));
}

#[test]
fn empty_strings_are_accepted_as_before() {
    assert!(parse_physical_scale(&json!({"axes": [{"name": "", "nm_per_px": 1}], "unit": ""})).is_some());
}

#[test]
fn every_other_shape_is_invalid() {
    for v in [
        json!({"axes": [], "unit": "nm"}),
        json!({"axes": [{"name": "x", "nm_per_px": 0.0}], "unit": "nm"}),
        json!({"axes": [{"name": "x", "nm_per_px": -1.0}], "unit": "nm"}),
        json!({"axes": [{"name": "x", "nm_per_px": "2"}], "unit": "nm"}),
        json!({"axes": [{"name": "x"}], "unit": "nm"}),
        json!({"axes": [{"nm_per_px": 5.0}], "unit": "nm"}),
        json!({"axes": [{"name": 1, "nm_per_px": 5.0}], "unit": "nm"}),
        json!({"axes": [{"name": "x", "nm_per_px": 2.0}, {"name": "y"}], "unit": "nm"}),
        json!({"axes": [{"name": "x", "nm_per_px": 2.0}]}),
        json!({"axes": [{"name": "x", "nm_per_px": 2.0}], "unit": 3}),
        json!({"axes": {"name": "x"}, "unit": "nm"}),
        json!({"unit": "nm"}),
        json!("nope"),
        json!(null),
    ] {
        assert_eq!(parse_physical_scale(&v), None, "{v}");
    }
}
