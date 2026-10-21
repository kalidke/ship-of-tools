use super::{
    downsample_oversize_raster, is_downsampleable_raster, PREVIEW_DOWNSAMPLE_MAX_DIM,
    PREVIEW_DOWNSAMPLE_TRIGGER,
};

#[test]
fn raster_mime_gate() {
    for m in [
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/tiff",
        "image/gif",
        "image/bmp",
    ] {
        assert!(is_downsampleable_raster(m), "{m} should be downsampleable");
    }
    for m in [
        "image/svg+xml",
        "text/plain",
        "application/pdf",
        "video/mp4",
    ] {
        assert!(!is_downsampleable_raster(m), "{m} must NOT be downsampled");
    }
}

#[test]
fn ships_raw_when_gates_fail() {
    // Under the size trigger → ship raw (None), no decode attempted.
    assert!(downsample_oversize_raster("image/png", &vec![0u8; 4096]).is_none());
    // Over the trigger but a non-raster mime → ship raw.
    let big = vec![0u8; PREVIEW_DOWNSAMPLE_TRIGGER + 1];
    assert!(downsample_oversize_raster("application/pdf", &big).is_none());
}

#[test]
fn downsizes_oversize_raster_to_cap() {
    use image::ImageEncoder;
    // Build a raster whose longest side exceeds the cap and whose PNG clears
    // the byte trigger (xorshift-noise defeats DEFLATE so it stays large).
    let (w, h) = (PREVIEW_DOWNSAMPLE_MAX_DIM + 800, 1600u32);
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    let mut s: u32 = 0x9e3779b9;
    for _ in 0..(w * h) {
        for _ in 0..4 {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            buf.push((s & 0xff) as u8);
        }
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&buf, w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    assert!(
        png.len() > PREVIEW_DOWNSAMPLE_TRIGGER,
        "noise PNG {} must exceed the {}-byte trigger to exercise downsample",
        png.len(),
        PREVIEW_DOWNSAMPLE_TRIGGER
    );

    let (out, scale) =
        downsample_oversize_raster("image/png", &png).expect("should downsample");
    assert!(out.len() < png.len(), "downsized bytes must be smaller");
    let (ow, oh) = image::ImageReader::new(std::io::Cursor::new(&out))
        .with_guessed_format()
        .unwrap()
        .into_dimensions()
        .unwrap();
    assert!(
        ow.max(oh) <= PREVIEW_DOWNSAMPLE_MAX_DIM,
        "longest side {} must be <= cap {}",
        ow.max(oh),
        PREVIEW_DOWNSAMPLE_MAX_DIM
    );
    assert_eq!(
        ow.max(oh),
        PREVIEW_DOWNSAMPLE_MAX_DIM,
        "longest side scaled to the cap"
    );
    // The returned scale is the linear factor that hit the cap, so it must
    // reproduce the observed shrink (used to rescale the scalebar, F1).
    assert!(scale > 0.0 && scale < 1.0, "downsample scale in (0,1): {scale}");
}

#[test]
fn rescale_physical_scale_multiplies_every_axis() {
    // A 2×-downsample (scale=0.5 → factor=2.0) must double every axis's
    // per-original-px nm_per_px to per-served-px, preserving anisotropy.
    let extras = Some(serde_json::json!({
        "physical_scale": {
            "axes": [
                {"name": "x", "nm_per_px": 2.0},
                {"name": "z", "nm_per_px": 5.0}
            ],
            "unit": "nm"
        }
    }));
    let out = super::rescale_physical_scale(extras, 2.0).unwrap();
    let axes = out["physical_scale"]["axes"].as_array().unwrap();
    assert_eq!(axes[0]["nm_per_px"].as_f64().unwrap(), 4.0);
    assert_eq!(axes[1]["nm_per_px"].as_f64().unwrap(), 10.0);
    // unit + names untouched.
    assert_eq!(out["physical_scale"]["unit"], "nm");
    assert_eq!(axes[0]["name"], "x");
}

#[test]
fn rescale_physical_scale_noop_without_scale() {
    // No physical_scale → returned unchanged (no panic, no spurious key).
    let extras = Some(serde_json::json!({"page": 3}));
    let out = super::rescale_physical_scale(extras, 2.0).unwrap();
    assert_eq!(out["page"], 3);
    assert!(out.get("physical_scale").is_none());
    // None in → None out.
    assert!(super::rescale_physical_scale(None, 2.0).is_none());
}
