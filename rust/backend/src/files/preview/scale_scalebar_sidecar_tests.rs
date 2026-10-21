use super::{merge_scale_sidecar, write_scale_sidecar_atomic};
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sot-scale-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn write_scale_sidecar_atomic_roundtrips_verbatim() {
    let dir = tmp("write");
    let img = dir.join("render.png");
    std::fs::write(&img, b"\x89PNG").unwrap();
    let scale = serde_json::json!({
        "axes": [{"name":"x","nm_per_px":2.0},{"name":"y","nm_per_px":2.0}],
        "unit": "nm"
    });
    let sidecar = dir.join("render.png.scale.json");
    write_scale_sidecar_atomic(&sidecar, &scale).expect("atomic write");
    // Written verbatim + readable back through the same merge path.
    let extras = merge_scale_sidecar(&img, "image/png", None);
    let ps = extras
        .as_ref()
        .and_then(|e| e.get("physical_scale"))
        .expect("physical_scale from the written sidecar");
    assert_eq!(ps["unit"], "nm");
    assert_eq!(ps["axes"][0]["nm_per_px"], 2.0);
    assert_eq!(ps["axes"][1]["name"], "y");
    // No temp file left behind (atomic temp+rename cleaned up).
    let leftover: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
        .collect();
    assert!(leftover.is_empty(), "temp file not cleaned: {leftover:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sidecar_sets_physical_scale_for_raster() {
    let dir = tmp("set");
    let img = dir.join("render.png");
    std::fs::write(&img, b"").unwrap();
    std::fs::write(
        dir.join("render.png.scale.json"),
        br#"{"axes":[{"name":"x","nm_per_px":2.0},{"name":"y","nm_per_px":2.0}],"unit":"nm"}"#,
    )
    .unwrap();
    let extras = merge_scale_sidecar(&img, "image/png", None);
    let ps = extras
        .as_ref()
        .and_then(|e| e.get("physical_scale"))
        .expect("physical_scale set from sidecar");
    assert_eq!(ps["unit"], "nm");
    assert_eq!(ps["axes"][0]["nm_per_px"], 2.0);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn no_sidecar_leaves_extras_untouched() {
    let dir = tmp("none");
    let img = dir.join("bare.png");
    std::fs::write(&img, b"").unwrap();
    assert!(merge_scale_sidecar(&img, "image/png", None).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn non_raster_mime_is_skipped() {
    let dir = tmp("skip");
    let f = dir.join("notes.txt");
    std::fs::write(&f, b"").unwrap();
    // A sidecar present but the mime isn't a raster → skipped, not read.
    std::fs::write(dir.join("notes.txt.scale.json"), b"{}").unwrap();
    assert!(merge_scale_sidecar(&f, "text/plain; charset=utf-8", None).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn merges_into_existing_extras_without_clobbering() {
    let dir = tmp("merge");
    let img = dir.join("m.png");
    std::fs::write(&img, b"").unwrap();
    std::fs::write(
        dir.join("m.png.scale.json"),
        br#"{"axes":[{"name":"x","nm_per_px":2}],"unit":"nm"}"#,
    )
    .unwrap();
    let extras = merge_scale_sidecar(&img, "image/png", Some(serde_json::json!({"page": 2})))
        .expect("some");
    assert_eq!(extras["page"], 2, "existing extras preserved");
    assert_eq!(extras["physical_scale"]["unit"], "nm");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn invalid_json_sidecar_is_ignored() {
    let dir = tmp("bad");
    let img = dir.join("b.png");
    std::fs::write(&img, b"").unwrap();
    std::fs::write(dir.join("b.png.scale.json"), b"not json{").unwrap();
    assert!(merge_scale_sidecar(&img, "image/png", None).is_none());
    std::fs::remove_dir_all(&dir).ok();
}
