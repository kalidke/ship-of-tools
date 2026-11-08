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

    // A folder at the picture's name is refused, and what is there stays.
    let picture = files.node_id_to_path("files:.sot/captures/ink-r3.png").unwrap();
    std::fs::create_dir_all(&picture).unwrap();
    assert_eq!(compose(&files, "r3", &stroke(InkBase::Canvas)).unwrap_err().0, PUBLISH_FAILED);
    assert!(picture.is_dir());
}

/// A link (a junction on Windows) at `at` to the folder `target`.
fn link_folder(target: &Path, at: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, at).unwrap();
    #[cfg(windows)]
    crate::agents::account_links::make_junction(target, at).unwrap();
}

/// A root with `.sot` or `.sot/captures` a link to a folder, wherever it leads: out of the root, or to `inner`.
/// Returns the root and the folder the link leads to, which must stay empty.
fn root_with_link(link_at: &str, outside: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    figure_png(root.path(), "fit.png", 40, 20);
    std::fs::create_dir(root.path().join("inner")).unwrap();
    let target = if link_at == "inner" { root.path().join("inner") } else { outside.to_path_buf() };
    if link_at == ".sot/captures" {
        std::fs::create_dir(root.path().join(".sot")).unwrap();
    }
    link_folder(&target, &root.path().join(if link_at == "inner" { ".sot" } else { link_at }));
    (root, target)
}

const LINKS: [&str; 3] = [".sot", ".sot/captures", "inner"];

#[test]
fn a_captures_folder_that_is_a_link_takes_no_write() {
    let outside = tempfile::tempdir().unwrap();
    for link_at in LINKS {
        let (root, target) = root_with_link(link_at, outside.path());
        let files = FilesMode::new(root.path().to_path_buf()).unwrap();
        let refused = compose(&files, "r1", &stroke(InkBase::Canvas)).unwrap_err();
        assert_eq!(refused.0, PUBLISH_FAILED, "{link_at}");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0, "{link_at}: wrote through the link");
    }
}

#[test]
fn a_crop_folder_that_is_a_link_takes_no_write() {
    use super::crop::crop_to_captures;
    let outside = tempfile::tempdir().unwrap();
    for link_at in LINKS {
        let (root, target) = root_with_link(link_at, outside.path());
        let files = FilesMode::new(root.path().to_path_buf()).unwrap();
        let src = root.path().join("fit.png");
        assert!(crop_to_captures(&files, &src, "fit", (0, 0, 10, 10)).is_err(), "{link_at}");
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0, "{link_at}: wrote through the link");
    }
}

#[test]
fn a_crop_is_a_png_of_the_rect_beneath_the_captures_folder() {
    use super::crop::crop_to_captures;
    let root = tempfile::tempdir().unwrap();
    let files = FilesMode::new(root.path().to_path_buf()).unwrap();
    figure_png(root.path(), "fit.png", 40, 20);
    let src = root.path().join("fit.png");
    let (out, x, y, w, h, sw, sh) = crop_to_captures(&files, &src, "fit", (5, 4, 10, 10)).unwrap();
    assert_eq!((x, y, w, h, sw, sh), (5, 4, 10, 10, 40, 20));
    assert_eq!(out.parent().unwrap(), files.node_id_to_path("files:.sot/captures").unwrap());
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(image::guess_format(&bytes).unwrap(), image::ImageFormat::Png);
    assert_eq!(image::load_from_memory(&bytes).unwrap().to_rgb8().dimensions(), (10, 10));
    // A rect over the edge is clamped, in the reply and in the picture.
    let (out, x, y, w, h, ..) = crop_to_captures(&files, &src, "fit", (35, 15, 10, 10)).unwrap();
    assert_eq!((x, y, w, h), (35, 15, 5, 5));
    assert_eq!(image::open(&out).unwrap().to_rgb8().dimensions(), (5, 5));
    // An origin past the edge is clamped to the last pixel.
    let (_, x, y, w, h, ..) = crop_to_captures(&files, &src, "fit", (100, 100, 5, 5)).unwrap();
    assert_eq!((x, y, w, h), (39, 19, 1, 1));
    // The decoder is picked from the bytes, not the name: a PNG under a name that says JPEG still decodes,
    // and bytes of no supported format answer decode_failed.
    let named = root.path().join("fit-as.jpg");
    std::fs::write(&named, std::fs::read(&src).unwrap()).unwrap();
    assert!(crop_to_captures(&files, &named, "fit-as", (0, 0, 1, 1)).is_ok());
    std::fs::write(root.path().join("junk.png"), b"not an image").unwrap();
    let junk = root.path().join("junk.png");
    assert_eq!(crop_to_captures(&files, &junk, "junk", (0, 0, 1, 1)).unwrap_err().0, "decode_failed");
}

#[test]
fn a_name_the_write_refuses_answers_as_a_node_id() {
    let root = tempfile::tempdir().unwrap();
    let files = FilesMode::new(root.path().to_path_buf()).unwrap();
    for id in ["files:a/../b", "files:a//b", "files:"] {
        let e = files.write(id, b"x").unwrap_err();
        assert!(format!("{e:#}").contains("node id cannot be written"), "{id}: {e:#}");
    }
    assert!(files.write("a/b", b"x").unwrap_err().to_string().contains("not a files-mode id"));
}

#[cfg(unix)]
#[test]
fn a_fifo_named_like_an_image_answers_at_once() {
    use super::crop::crop_to_captures;
    let root = tempfile::tempdir().unwrap();
    let files = FilesMode::new(root.path().to_path_buf()).unwrap();
    let fifo = root.path().join("pipe.png");
    let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let base = InkBase::Figure { node_id: "files:pipe.png".into() };
        let composed = compose(&files, "r1", &stroke(base)).unwrap_err().0;
        let cropped = crop_to_captures(&files, &fifo, "pipe", (0, 0, 1, 1)).unwrap_err().0;
        tx.send((composed, cropped)).ok();
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("a FIFO held the call");
    assert_eq!((got.0, got.1.as_str()), (NOT_AN_IMAGE, "decode_failed"));
}
