use super::{merge_png_phys_scale, png_phys_nm_per_px};

const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// A well-formed PNG chunk: len + type + data + the REAL CRC over
/// type+data. The walk validates CRCs now, so a dummy value would make
/// every fixture read as corrupt.
fn chunk(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut covered = ty.to_vec();
    covered.extend_from_slice(data);
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(&covered);
    v.extend_from_slice(&crc32fast::hash(&covered).to_be_bytes());
    v
}

/// Same, with a deliberately wrong CRC.
fn chunk_bad_crc(ty: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = chunk(ty, data);
    let n = v.len();
    v[n - 1] ^= 0xFF;
    v
}

fn phys_data(ppm_x: u32, ppm_y: u32, unit: u8) -> Vec<u8> {
    let mut d = ppm_x.to_be_bytes().to_vec();
    d.extend_from_slice(&ppm_y.to_be_bytes());
    d.push(unit);
    d
}

fn png_with(chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut v = SIG.to_vec();
    for c in chunks {
        v.extend_from_slice(c);
    }
    v
}

/// 100_000_000 px/m == 10 nm/px exactly.
fn isotropic_10nm() -> Vec<u8> {
    png_with(&[
        chunk(b"IHDR", &[0; 13]),
        chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)),
        chunk(b"IDAT", &[0; 4]),
        chunk(b"IEND", &[]),
    ])
}

#[test]
fn metre_unit_phys_resolves_exactly() {
    assert_eq!(png_phys_nm_per_px(&isotropic_10nm()), Some((10.0, 10.0)));
    // Anisotropic still READS as two axes — suppression is the merge
    // layer's job, so the reader stays an honest report of the file.
    let aniso = png_with(&[
        chunk(b"pHYs", &phys_data(100_000_000, 200_000_000, 1)),
        chunk(b"IEND", &[]),
    ]);
    assert_eq!(png_phys_nm_per_px(&aniso), Some((10.0, 5.0)));
}

#[test]
fn aspect_only_zero_density_and_missing_phys_resolve_none() {
    // unit 0 = aspect ratio only (dimensionless).
    let aspect = png_with(&[chunk(b"pHYs", &phys_data(2, 1, 0)), chunk(b"IEND", &[])]);
    assert_eq!(png_phys_nm_per_px(&aspect), None);
    // Unknown unit byte is not metres either.
    let unknown = png_with(&[chunk(b"pHYs", &phys_data(1000, 1000, 7)), chunk(b"IEND", &[])]);
    assert_eq!(png_phys_nm_per_px(&unknown), None);
    // Either axis zero is a division we must not do.
    for d in [phys_data(0, 5, 1), phys_data(5, 0, 1)] {
        let zero = png_with(&[chunk(b"pHYs", &d), chunk(b"IEND", &[])]);
        assert_eq!(png_phys_nm_per_px(&zero), None);
    }
    let none = png_with(&[chunk(b"IHDR", &[0; 13]), chunk(b"IEND", &[])]);
    assert_eq!(png_phys_nm_per_px(&none), None);
}

#[test]
fn malformed_streams_resolve_none() {
    assert_eq!(png_phys_nm_per_px(b"not a png"), None);
    assert_eq!(png_phys_nm_per_px(b""), None);
    // pHYs after IDAT violates the spec — the walk stops at IDAT.
    let late = png_with(&[
        chunk(b"IDAT", &[0; 4]),
        chunk(b"pHYs", &phys_data(1_000_000, 1_000_000, 1)),
    ]);
    assert_eq!(png_phys_nm_per_px(&late), None);
    // A declared length that overruns the buffer, at every truncation
    // point of a valid file (never panics, never reads out of bounds).
    let full = isotropic_10nm();
    for cut in 1..full.len() {
        let _ = png_phys_nm_per_px(&full[..cut]);
    }
    // u32::MAX length: `checked_add` must catch it, not wrap.
    let mut huge = SIG.to_vec();
    huge.extend_from_slice(&u32::MAX.to_be_bytes());
    huge.extend_from_slice(b"pHYs");
    huge.extend_from_slice(&[0; 16]);
    assert_eq!(png_phys_nm_per_px(&huge), None);
}

#[test]
fn spec_violations_are_rejected_not_skipped() {
    let good = chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1));
    // Corrupt CRC: never trust a calibration we can't verify.
    let bad_crc = png_with(&[
        chunk_bad_crc(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)),
        chunk(b"IEND", &[]),
    ]);
    assert_eq!(png_phys_nm_per_px(&bad_crc), None);
    // Wrong length is malformed — and must NOT fall through to a later,
    // well-formed pHYs, which is how a bad file could pick its own scale.
    let short = png_with(&[
        chunk(b"pHYs", &phys_data(1_000_000, 1_000_000, 1)[..8]),
        good.clone(),
        chunk(b"IEND", &[]),
    ]);
    assert_eq!(png_phys_nm_per_px(&short), None);
    // At most one pHYs per the spec; a duplicate is malformed even when
    // the first one was perfectly good.
    let dup = png_with(&[good.clone(), good.clone(), chunk(b"IEND", &[])]);
    assert_eq!(png_phys_nm_per_px(&dup), None);
    // ...including a duplicate after a legal aspect-only first chunk.
    let dup_after_aspect = png_with(&[
        chunk(b"pHYs", &phys_data(2, 1, 0)),
        good.clone(),
        chunk(b"IEND", &[]),
    ]);
    assert_eq!(png_phys_nm_per_px(&dup_after_aspect), None);
}

#[test]
fn walk_is_bounded_and_skips_large_ancillary_chunks() {
    // A big iCCP/eXIf before pHYs is skipped by declared length, never
    // scanned byte-by-byte.
    let big = png_with(&[
        chunk(b"iCCP", &vec![0u8; 512 * 1024]),
        chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)),
        chunk(b"IEND", &[]),
    ]);
    assert_eq!(png_phys_nm_per_px(&big), Some((10.0, 10.0)));
    // Zero-length chunks still advance (12 bytes each), so this
    // terminates — on the budget, not on a scan of the whole file.
    let mut flood = SIG.to_vec();
    for _ in 0..20_000 {
        flood.extend_from_slice(&chunk(b"tEXt", &[]));
    }
    flood.extend_from_slice(&chunk(b"pHYs", &phys_data(100_000_000, 100_000_000, 1)));
    assert_eq!(png_phys_nm_per_px(&flood), None);
}

#[test]
fn merge_fills_only_when_sidecar_did_not() {
    let png = isotropic_10nm();
    // No prior extras → pHYs fills, sidecar schema shape.
    let merged = merge_png_phys_scale(&png, "image/png", None, true).unwrap();
    let axes = &merged["physical_scale"]["axes"];
    assert_eq!(axes[0]["name"], "x");
    assert_eq!(axes[0]["nm_per_px"], 10.0);
    assert_eq!(axes[1]["name"], "y");
    assert_eq!(axes[1]["nm_per_px"], 10.0);
    assert_eq!(merged["physical_scale"]["unit"], "nm");
    // A valid scale already resolved → untouched (exact floats win).
    let sidecar = serde_json::json!({"physical_scale": {"axes": [{"name": "x", "nm_per_px": 3.0}], "unit": "nm"}});
    let kept = merge_png_phys_scale(&png, "image/png", Some(sidecar.clone()), true);
    assert_eq!(kept, Some(sidecar));
    // Non-PNG mime → untouched.
    assert_eq!(merge_png_phys_scale(&png, "image/jpeg", None, true), None);
    // Unrelated extras are preserved alongside the new key.
    let other = serde_json::json!({ "page_count": 3 });
    let both = merge_png_phys_scale(&png, "image/png", Some(other), true).unwrap();
    assert_eq!(both["page_count"], 3);
    assert_eq!(both["physical_scale"]["axes"][0]["nm_per_px"], 10.0);
}

/// The tiers as `build_preview_payload` runs them: sidecar, then embedded `pHYs`.
fn resolved(
    img: &std::path::Path,
    png: &[u8],
    extras: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let extras = super::merge_scale_sidecar(img, "image/png", extras);
    merge_png_phys_scale(png, "image/png", extras, true)
}

#[test]
fn invalid_sidecar_preserves_png_phys() {
    let dir = std::env::temp_dir().join(format!("sot-scale-invalid-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("render.png");
    let sidecar = dir.join("render.png.scale.json");
    let png = isotropic_10nm();
    for bad in [
        r#"{"axes":[],"unit":"nm"}"#,
        r#"{"axes":[{"name":"x","nm_per_px":0}],"unit":"nm"}"#,
        r#"{"axes":[{"name":"x","nm_per_px":-2}],"unit":"nm"}"#,
        r#"{"axes":[{"name":"x","nm_per_px":"2"}],"unit":"nm"}"#,
        r#"{"axes":[{"nm_per_px":2}],"unit":"nm"}"#,
        r#"{"axes":[{"name":"x","nm_per_px":2}]}"#,
        r#"{"axes":[{"name":"x","nm_per_px":2}],"unit":7}"#,
        r#"null"#,
        r#""scale""#,
    ] {
        std::fs::write(&sidecar, bad).unwrap();
        let got = resolved(&img, &png, Some(serde_json::json!({"page_count": 3}))).unwrap();
        assert_eq!(got["physical_scale"]["axes"][0]["nm_per_px"], 10.0, "{bad}");
        assert_eq!(got["page_count"], 3, "{bad}");
    }
    // A valid sidecar, anisotropic or with empty strings, still outranks the embedded density.
    for good in [
        r#"{"axes":[{"name":"x","nm_per_px":2},{"name":"z","nm_per_px":8}],"unit":"nm"}"#,
        r#"{"axes":[{"name":"","nm_per_px":2}],"unit":""}"#,
    ] {
        std::fs::write(&sidecar, good).unwrap();
        let got = resolved(&img, &png, None).unwrap();
        assert_eq!(got["physical_scale"]["axes"][0]["nm_per_px"], 2.0, "{good}");
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn invalid_existing_scale_does_not_hide_png_phys() {
    let png = isotropic_10nm();
    for bad in [
        serde_json::json!({"physical_scale": {"axes": [], "unit": "nm"}}),
        serde_json::json!({"physical_scale": null}),
        serde_json::json!({"physical_scale": {"axes": [{"name": "x", "nm_per_px": 0}], "unit": "nm"}}),
    ] {
        let got = merge_png_phys_scale(&png, "image/png", Some(bad), true).unwrap();
        assert_eq!(got["physical_scale"]["axes"][0]["nm_per_px"], 10.0);
    }
}

#[test]
fn plugin_output_is_never_read_for_embedded_scale() {
    // Same bytes, same mime — only provenance differs. A plugin's blob
    // may be re-encoded, resized, or carry a rasterizer's own render DPI
    // (pdftoppm stamps 144), so its pHYs is not the subject's scale.
    let png = isotropic_10nm();
    assert!(merge_png_phys_scale(&png, "image/png", None, true).is_some());
    assert_eq!(merge_png_phys_scale(&png, "image/png", None, false), None);
}

#[test]
fn anisotropic_phys_is_suppressed_not_half_rendered() {
    // The FE renders one bar from axes[0] by design, so unequal axes read
    // automatically off a file would silently label the image with its x
    // scale alone. Suppress rather than mislabel.
    let aniso = png_with(&[
        chunk(b"pHYs", &phys_data(100_000_000, 200_000_000, 1)),
        chunk(b"IEND", &[]),
    ]);
    assert_eq!(png_phys_nm_per_px(&aniso), Some((10.0, 5.0)));
    assert_eq!(merge_png_phys_scale(&aniso, "image/png", None, true), None);
    // ...but an explicitly anisotropic SIDECAR is a deliberate human act
    // and still passes through untouched.
    let sidecar = serde_json::json!({"physical_scale": {
        "axes": [{"name":"x","nm_per_px":10.0},{"name":"y","nm_per_px":5.0}], "unit":"nm"}});
    assert_eq!(
        merge_png_phys_scale(&aniso, "image/png", Some(sidecar.clone()), true),
        Some(sidecar)
    );
}
