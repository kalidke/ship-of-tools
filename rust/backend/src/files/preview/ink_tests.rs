use super::ink::compose;
use crate::files::tree::FilesMode;
use image::ImageEncoder;
use sot_protocol::ink::{INK_RGB, CANVAS_DIMS};
use sot_protocol::ops::selection_code::{NOT_AN_IMAGE, PUBLISH_FAILED, TOO_LARGE};
use sot_protocol::ops::{Drawing, InkBase};
use std::path::Path;

const FIGURE: [u8; 3] = [10, 200, 30];

fn figure_png(dir: &Path, name: &str, w: u32, h: u32) -> String {
    image::RgbImage::from_pixel(w, h, image::Rgb(FIGURE)).save(dir.join(name)).unwrap();
    format!("files:{name}")
}

/// One horizontal stroke through the middle of the base, from 0.4 to 0.6 of its width.
fn stroke(base: InkBase) -> Drawing {
    let (y, a, b) = (32768, (0.4 * 65535.0) as u16, (0.6 * 65535.0) as u16);
    Drawing { base, strokes: vec![vec![[a, y], [b, y]]] }
}

fn pixel(png: &Path, fx: f32, fy: f32) -> [u8; 3] {
    let img = image::open(png).unwrap().to_rgb8();
    let (x, y) = ((fx * img.width() as f32) as u32, (fy * img.height() as f32) as u32);
    img.get_pixel(x, y).0
}

#[test]
fn the_picture_is_the_base_with_its_ink() {
    let dir = tempfile::tempdir().unwrap();
    let files = FilesMode::new(dir.path().to_path_buf()).unwrap();

    // A figure over the raster cap is scaled to `raster_dims`; its far pixels stay, the stroke's are ink.
    let node_id = figure_png(dir.path(), "fit.png", 4000, 2000);
    let out = compose(&files, "r1", &stroke(InkBase::Figure { node_id })).unwrap();
    assert_eq!(out, files.node_id_to_path("files:.sot/captures/ink-r1.png").unwrap());
    assert_eq!(image::image_dimensions(&out).unwrap(), (2000, 1000));
    assert_eq!(pixel(&out, 0.5, 0.5), INK_RGB);
    assert_eq!(pixel(&out, 0.1, 0.1), FIGURE);

    // A canvas is white at the canvas size, with the same ink.
    let out = compose(&files, "r2", &stroke(InkBase::Canvas)).unwrap();
    assert_eq!(image::image_dimensions(&out).unwrap(), CANVAS_DIMS);
    assert_eq!(pixel(&out, 0.5, 0.5), INK_RGB);
    assert_eq!(pixel(&out, 0.1, 0.1), [255; 3]);

    // Over 40 megapixels is refused before a decode: this image is cut short, so a decode of it would answer
    // `not_an_image`.
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&vec![0u8; 7000 * 7000], 7000, 7000, image::ExtendedColorType::L8)
        .unwrap();
    std::fs::write(dir.path().join("huge.png"), &png[..png.len() / 2]).unwrap();
    let huge = InkBase::Figure { node_id: "files:huge.png".into() };
    assert_eq!(compose(&files, "r1", &stroke(huge)).unwrap_err().0, TOO_LARGE);

    // Bytes that are no image, and a file that is not one by its name.
    std::fs::write(dir.path().join("junk.png"), b"not a png").unwrap();
    std::fs::write(dir.path().join("note.txt"), b"text").unwrap();
    for name in ["files:junk.png", "files:note.txt"] {
        let base = InkBase::Figure { node_id: name.into() };
        assert_eq!(compose(&files, "r1", &stroke(base)).unwrap_err().0, NOT_AN_IMAGE, "{name}");
    }

    // Only a file whose name says image is read as one, however its bytes decode.
    image::RgbImage::from_pixel(8, 8, image::Rgb(FIGURE)).save_with_format(dir.path().join("fig.txt"), image::ImageFormat::Png).unwrap();
    let named = InkBase::Figure { node_id: "files:fig.txt".into() };
    assert_eq!(compose(&files, "r1", &stroke(named)).unwrap_err().0, NOT_AN_IMAGE);

    // The ink's edge is coverage-blended over the figure: some pixels are neither figure nor ink.
    let node_id = figure_png(dir.path(), "edge.png", 400, 200);
    let out = compose(&files, "r4", &stroke(InkBase::Figure { node_id })).unwrap();
    let blended = image::open(&out).unwrap().to_rgb8().pixels().filter(|p| p.0 != FIGURE && p.0 != INK_RGB).count();
    assert!(blended > 0, "an antialiased edge has blended pixels");

    // The write is whole or none: where the temporary file cannot be made the old picture stays as it was.
    let picture = files.node_id_to_path("files:.sot/captures/ink-r3.png").unwrap();
    std::fs::write(&picture, b"OLD").unwrap();
    std::fs::create_dir(picture.with_extension("png.tmp")).unwrap();
    assert_eq!(compose(&files, "r3", &stroke(InkBase::Canvas)).unwrap_err().0, PUBLISH_FAILED);
    assert_eq!(std::fs::read(&picture).unwrap(), b"OLD");
}

#[cfg(unix)]
#[test]
fn a_captures_folder_that_leads_out_of_the_root_takes_no_write() {
    use super::crop::crop_to_captures;
    let outside = tempfile::tempdir().unwrap();
    for link_at in [".sot", ".sot/captures"] {
        let root = tempfile::tempdir().unwrap();
        let files = FilesMode::new(root.path().to_path_buf()).unwrap();
        figure_png(root.path(), "fit.png", 40, 20);
        if link_at == ".sot" {
            std::os::unix::fs::symlink(outside.path(), root.path().join(".sot")).unwrap();
        } else {
            std::fs::create_dir(root.path().join(".sot")).unwrap();
            std::os::unix::fs::symlink(outside.path(), root.path().join(".sot/captures")).unwrap();
        }
        let refused = compose(&files, "r1", &stroke(InkBase::Canvas)).unwrap_err();
        assert_eq!(refused.0, PUBLISH_FAILED, "{link_at}");
        let src = root.path().join("fit.png");
        assert!(crop_to_captures(&files, &src, "fit", (0, 0, 10, 10)).is_err(), "{link_at}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0, "{link_at}: wrote outside the root");
    }
}

#[cfg(windows)]
#[test]
fn a_captures_junction_that_leads_out_of_the_root_takes_no_write() {
    use crate::agents::account_links::make_junction;
    let outside = tempfile::tempdir().unwrap();
    for link_at in [".sot", ".sot/captures"] {
        let root = tempfile::tempdir().unwrap();
        let files = FilesMode::new(root.path().to_path_buf()).unwrap();
        if link_at == ".sot" {
            make_junction(outside.path(), &root.path().join(".sot")).unwrap();
        } else {
            std::fs::create_dir(root.path().join(".sot")).unwrap();
            make_junction(outside.path(), &root.path().join(".sot").join("captures")).unwrap();
        }
        assert_eq!(compose(&files, "r1", &stroke(InkBase::Canvas)).unwrap_err().0, PUBLISH_FAILED, "{link_at}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0, "{link_at}: wrote outside the root");
    }
}
