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
#[path = "physical_scale_tests.rs"]
mod tests;
