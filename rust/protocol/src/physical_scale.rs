//! The physical scale of a raster preview (ADR 0034): the one definition of a valid `physical_scale` JSON value.

use serde_json::Value;

/// One axis of a scale: its label and the physical length of one source pixel.
#[derive(Debug, Clone, PartialEq)]
pub struct ScaleAxis {
    pub name: String,
    pub nm_per_px: f64,
}

/// A valid `{axes:[{name, nm_per_px}], unit}`; `axes[0]` is the horizontal image axis.
#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalScale {
    pub axes: Vec<ScaleAxis>,
    pub unit: String,
}

/// Parse a `physical_scale` value. It is valid when `unit` is a string and `axes` is a nonempty array whose
/// every axis has a string `name` and a finite, positive numeric `nm_per_px`; anything else is `None`, never a
/// partial scale. Empty strings for the unit or a name are accepted.
pub fn parse_physical_scale(v: &Value) -> Option<PhysicalScale> {
    let unit = v.get("unit")?.as_str()?.to_string();
    let axes = v
        .get("axes")?
        .as_array()?
        .iter()
        .map(|axis| {
            let nm_per_px = axis.get("nm_per_px")?.as_f64().filter(|n| n.is_finite() && *n > 0.0)?;
            Some(ScaleAxis { name: axis.get("name")?.as_str()?.to_string(), nm_per_px })
        })
        .collect::<Option<Vec<_>>>()?;
    (!axes.is_empty()).then_some(PhysicalScale { axes, unit })
}

#[cfg(test)]
mod tests {
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
}
