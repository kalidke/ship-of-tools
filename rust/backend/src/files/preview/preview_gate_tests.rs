use super::{build_preview_payload, fit_plugin_output, is_bounded_output_plugin, read_bytes_preview, MAX_BOUNDED_PICTURE_BYTES, PREVIEW_BYTE_CAP};
use std::path::Path;

#[test]
fn every_video_extension_in_any_case_is_bounded() {
    for p in ["a.mp4", "A.MP4", "b.WebM", "c.mov", "d.MKV", "e.m4v"] {
        assert!(is_bounded_output_plugin(Path::new(p)), "{p} should be exempt");
    }
    for p in ["clip.avi", "song.mp3", "x.gif", "noext", "mp4"] {
        assert!(!is_bounded_output_plugin(Path::new(p)), "{p} should be gated");
    }
}

#[test]
fn the_bytes_reader_answers_a_video_with_the_unavailable_note() {
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("clip.MOV");
    std::fs::write(&video, b"\x00\x01\x02").unwrap();
    let (mime, body) = read_bytes_preview(&video, "n1").unwrap();
    assert_eq!(mime, "text/markdown");
    assert!(body.starts_with(b"# video preview unavailable"));
    let other = dir.path().join("clip.avi");
    std::fs::write(&other, b"raw-bytes").unwrap();
    let (_, body) = read_bytes_preview(&other, "n2").unwrap();
    assert_eq!(body, b"raw-bytes");
}

#[test]
fn bounded_output_plugins_exempt_from_size_gate() {
    // HDF5 + video are metadata/poster-only → bounded output → must NOT be
    // skipped on big input (the multi-GB .h5 freeze bug).
    for p in [
        "data.h5",
        "scan.hdf5",
        "old.hdf",
        "DATA.H5",
        "clip.mp4",
        "v.mkv",
    ] {
        assert!(
            is_bounded_output_plugin(Path::new(p)),
            "{p} should be exempt"
        );
    }
    // Plugins whose output scales with input (or plain files) stay gated.
    for p in ["mod.jl", "notes.txt", "data.json", "big.csv", "noext"] {
        assert!(
            !is_bounded_output_plugin(Path::new(p)),
            "{p} should be gated"
        );
    }
}

/// A PDF page rasterized for a zoomed pane, or a video's poster, past the text cap ships whole: the bytes-level reader
/// would answer with the file's own bytes (a PDF's as `text/plain`, which the window only summarizes). Text is still cut
/// at the cap, and token JSON past it still goes to the bytes-level reader.
#[test]
fn a_bounded_plugins_picture_past_the_text_cap_ships_whole() {
    let picture = vec![0x89; PREVIEW_BYTE_CAP + 1];
    for path in ["paper.pdf", "Paper.PDF", "clip.mp4"] {
        let shipped = fit_plugin_output(Path::new(path), "n", "image/png", picture.clone());
        assert!(shipped.as_deref() == Some(&picture[..]), "{path}: shipped {:?} bytes", shipped.map(|b| b.len()));
    }
    assert!(fit_plugin_output(Path::new("mod.jl"), "n", "application/vnd.sot.tokens+json", picture.clone()).is_none());
    let text = fit_plugin_output(Path::new("data.h5"), "n", "text/markdown", picture.clone()).expect("text is cut, not dropped");
    assert_eq!(text.len(), PREVIEW_BYTE_CAP);
    let huge = vec![0x89; MAX_BOUNDED_PICTURE_BYTES + 1];
    assert!(fit_plugin_output(Path::new("paper.pdf"), "n", "image/png", huge).is_none());
}

#[tokio::test]
async fn microscope_header_reaches_preview_with_scale_through_a_file_and_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let row = dir.path().join("row");
    std::fs::create_dir(&row).unwrap();
    let path = row.join("microscope.png");
    let mut png = include_bytes!("fixtures-header.bin").to_vec();
    let append_chunk = |png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(data);
        png.extend_from_slice(&crc.finalize().to_be_bytes());
    };
    // Match the observed file size: above the plugin input gate and below the backend downsample trigger.
    // Pixel data is immaterial to this bytes/metadata path; each appended chunk has its real CRC.
    let data = vec![0; 11_903_190 - png.len() - 24];
    append_chunk(&mut png, b"IDAT", &data);
    append_chunk(&mut png, b"IEND", &[]);
    std::fs::write(&path, &png).unwrap();
    let workspace = |root| crate::rows::Workspace::meta_only(
        "ws-preview".into(), "preview".into(), "Preview".into(), root,
        "preview".into(), 0, false, "none".into(), String::new(), String::new(),
    );
    let direct = workspace(row);
    let mut cases = vec![(direct, "files:microscope.png")];
    #[cfg(unix)]
    {
        // A second row reads the very same file through a link outside its root, as with external storage.
        let linked_row = dir.path().join("linked-row");
        std::fs::create_dir(&linked_row).unwrap();
        std::os::unix::fs::symlink(&path, linked_row.join("microscope.png")).unwrap();
        cases.push((workspace(linked_row), "files:microscope.png"));
    }
    for (ws, node_id) in cases.drain(..) {
        let req = sot_protocol::PreviewGetReq {
            node_id: node_id.into(), workspace_id: None, page: None, fit_w: None, fit_h: None,
        };
        let (mime, bytes, extras) = build_preview_payload(&ws, &req).await.unwrap().unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(bytes, png);
        let extras = extras.expect("the preview reply carries the embedded physical scale");
        let scale = sot_protocol::physical_scale::parse_physical_scale(&extras["physical_scale"]).unwrap();
        assert_eq!(scale.unit, "nm");
        assert_eq!(scale.axes.len(), 2);
        for (axis, name) in scale.axes.iter().zip(["x", "y"]) {
            assert_eq!(axis.name, name);
            assert!((axis.nm_per_px - 4.7785).abs() < 1e-4, "{axis:?}");
        }
    }
}
