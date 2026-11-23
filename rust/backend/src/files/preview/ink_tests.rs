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
    let out = compose(&files, "r1", &stroke(InkBase::Figure { node_id, page: None }), None).unwrap();
    assert_eq!(out, files.node_id_to_path("files:.sot/captures/ink-r1.png").unwrap());
    assert_eq!(image::image_dimensions(&out).unwrap(), (2000, 1000));
    assert_eq!(pixel(&out, 0.5, 0.5), INK_RGB);
    assert_eq!(pixel(&out, 0.1, 0.1), FIGURE);

    // A canvas is white at the canvas size, with the same ink.
    let out = compose(&files, "r2", &stroke(InkBase::Canvas), None).unwrap();
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
    let huge = InkBase::Figure { node_id: "files:huge.png".into(), page: None };
    assert_eq!(compose(&files, "r1", &stroke(huge), None).unwrap_err().0, TOO_LARGE);

    // Bytes that are no image, and a file that is not one by its name.
    std::fs::write(dir.path().join("junk.png"), b"not a png").unwrap();
    std::fs::write(dir.path().join("note.txt"), b"text").unwrap();
    for name in ["files:junk.png", "files:note.txt"] {
        let base = InkBase::Figure { node_id: name.into(), page: None };
        assert_eq!(compose(&files, "r1", &stroke(base), None).unwrap_err().0, NOT_AN_IMAGE, "{name}");
    }

    // Only a file whose name says image is read as one, however its bytes decode.
    image::RgbImage::from_pixel(8, 8, image::Rgb(FIGURE)).save_with_format(dir.path().join("fig.txt"), image::ImageFormat::Png).unwrap();
    let named = InkBase::Figure { node_id: "files:fig.txt".into(), page: None };
    assert_eq!(compose(&files, "r1", &stroke(named), None).unwrap_err().0, NOT_AN_IMAGE);

    // The ink's edge is coverage-blended over the figure: some pixels are neither figure nor ink.
    let node_id = figure_png(dir.path(), "edge.png", 400, 200);
    let out = compose(&files, "r4", &stroke(InkBase::Figure { node_id, page: None }), None).unwrap();
    let blended = image::open(&out).unwrap().to_rgb8().pixels().filter(|p| p.0 != FIGURE && p.0 != INK_RGB).count();
    assert!(blended > 0, "an antialiased edge has blended pixels");

    // A folder at the picture's name is refused, and what is there stays.
    let picture = files.node_id_to_path("files:.sot/captures/ink-r3.png").unwrap();
    std::fs::create_dir_all(&picture).unwrap();
    assert_eq!(compose(&files, "r3", &stroke(InkBase::Canvas), None).unwrap_err().0, PUBLISH_FAILED);
    assert!(picture.is_dir());
}

const PAGE: [u8; 3] = [20, 40, 220];

/// A PNG `w` by `h` of the page color, as a kernel's plugin would draw a page.
pub(crate) fn page_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut png = Vec::new();
    let page = image::RgbImage::from_pixel(w, h, image::Rgb(PAGE));
    image::codecs::png::PngEncoder::new(&mut png).write_image(page.as_raw(), w, h, image::ExtendedColorType::Rgb8).unwrap();
    png
}

/// A picture over a page is the page's bytes scaled to the wire's raster with the ink over them, and the paged file
/// itself is never read as a picture: a page's base with no page bytes is refused as its node is.
#[test]
fn a_pages_picture_is_the_page_with_its_ink() {
    let dir = tempfile::tempdir().unwrap();
    let files = FilesMode::new(dir.path().to_path_buf()).unwrap();
    std::fs::write(dir.path().join("doc.pdf"), b"%PDF-1.4").unwrap();
    let base = || InkBase::Figure { node_id: "files:doc.pdf".into(), page: Some(2) };
    let out = compose(&files, "p1", &stroke(base()), Some(page_bytes(3000, 4000).as_slice())).unwrap();
    assert_eq!(image::image_dimensions(&out).unwrap(), (1500, 2000));
    assert_eq!(pixel(&out, 0.5, 0.5), INK_RGB);
    assert_eq!(pixel(&out, 0.1, 0.1), PAGE);
    assert_eq!(compose(&files, "p2", &stroke(base()), None).unwrap_err().0, NOT_AN_IMAGE);
    assert_eq!(compose(&files, "p2", &stroke(base()), Some(b"not a png".as_slice())).unwrap_err().0, NOT_AN_IMAGE);
    // by a rule, not by the node's name: an image file named as a page's base is never read as the picture
    figure_png(dir.path(), "a.png", 40, 20);
    let named = InkBase::Figure { node_id: "files:a.png".into(), page: Some(2) };
    assert_eq!(compose(&files, "p2", &stroke(named), None).unwrap_err().0, NOT_AN_IMAGE);
    assert!(!dir.path().join(".sot/captures/ink-p2.png").exists(), "a refused page writes no picture");
}

/// A row in `project` whose kernel is a stand-in that answers every `file.preview` with `payload` and appends each
/// request line it reads to `<dir>/asked`.
#[cfg(unix)]
pub(crate) fn row_with_kernel(dir: &Path, project: &Path, id: &str, kind: &str, payload: serde_json::Value) -> crate::rows::Workspace {
    let (reply, stub, kernel_project) = (dir.join("reply"), dir.join("stub-julia"), dir.join("kp"));
    std::fs::write(&reply, format!("{}\n", serde_json::json!({"id": 0, "payload": payload}))).unwrap();
    let script = format!(
        "#!/bin/sh\nread -r l\necho '{{\"id\":1,\"payload\":{{\"protocol\":1}}}}'\nwhile read -r l; do\nprintf '%s\\n' \"$l\" >> {asked}\n\
         id=$(printf '%s' \"$l\" | sed 's/.*\"id\":\\([0-9]*\\).*/\\1/')\nsed \"s/\\\"id\\\":0/\\\"id\\\":$id/\" {reply}\ndone\n",
        asked = dir.join("asked").display(),
        reply = reply.display()
    );
    sot_platform::test_exec::write_executable(&stub, script);
    std::fs::create_dir(&kernel_project).unwrap();
    crate::sidecars::kernel::tests::STUB_BIN.lock().unwrap().push((kernel_project.clone(), stub.to_string_lossy().into_owned()));
    let mut row = crate::rows::Workspace::from_label(id, project.to_path_buf(), false, kind.into(), String::new(), String::new());
    row.workspace_id = id.to_string();
    row.set_kernel(crate::sidecars::kernel::Kernel::new(kernel_project, project.to_path_buf()));
    row
}

/// What a kernel's plugin answers for a page it drew: a picture and the page it says it drew.
#[cfg(unix)]
pub(crate) fn drawn_page(page: u32) -> serde_json::Value {
    use base64::Engine as _;
    let blob = base64::engine::general_purpose::STANDARD.encode(page_bytes(30, 40));
    serde_json::json!({"matched": true, "mime": "image/png", "blob_base64": blob, "extras": {"page": page, "page_count": 2}})
}

/// The page under a drawing is the one the row's kernel draws for the page asked, fitted to the wire's raster. A page
/// the kernel did not draw (it clamps a page past the file's end and says which it drew), a note in place of a
/// picture and a file no plugin pages are refused, so ink is never put over another page.
#[cfg(unix)]
#[tokio::test]
async fn a_page_is_drawn_by_the_kernel_and_a_page_it_did_not_draw_is_refused() {
    use super::ink::page_png;
    use sot_protocol::ops::selection_code::BAD_NODE_ID;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("doc.pdf"), b"%PDF-1.4").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let ws = row_with_kernel(dir.path(), project.path(), "ws-page", "claude", drawn_page(2));
    assert_eq!(page_png(&ws, "files:doc.pdf", 2).await.unwrap(), page_bytes(30, 40));
    let asked = std::fs::read_to_string(dir.path().join("asked")).unwrap();
    let asked: serde_json::Value = serde_json::from_str(asked.lines().next().unwrap()).unwrap();
    assert_eq!(asked["op"], "file.preview");
    assert_eq!(asked["payload"]["params"], serde_json::json!({"page": 2, "fit_w": 2000, "fit_h": 2000}));
    assert_eq!(page_png(&ws, "files:doc.pdf", 9).await.unwrap_err().0, NOT_AN_IMAGE, "the kernel drew page 2, not 9");
    assert_eq!(page_png(&ws, "files:../out.pdf", 2).await.unwrap_err().0, BAD_NODE_ID, "a node outside the root");
    let note = serde_json::json!({"matched": true, "mime": "text/markdown", "blob_base64": "IyBwZGY=", "extras": {"page": 2}});
    for (name, payload) in [("note", note), ("unmatched", serde_json::json!({"matched": false}))] {
        let dir = tempfile::tempdir().unwrap();
        let ws = row_with_kernel(dir.path(), project.path(), "ws-page", "claude", payload);
        assert_eq!(page_png(&ws, "files:doc.pdf", 2).await.unwrap_err().0, NOT_AN_IMAGE, "{name}");
    }
}

/// The kernel is asked only for a page from 1 of a regular file named `.pdf`: a text file with a page (its own plugin
/// would read it), a FIFO and a folder named `.pdf` (the kernel's tool would open the first and wait) and page 0 are
/// each refused with the kernel never asked, though this kernel would answer any request with a picture.
#[cfg(unix)]
#[tokio::test]
async fn a_base_that_is_no_page_of_a_pdf_file_never_reaches_the_kernel() {
    use super::ink::page_png;
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("doc.PDF"), b"%PDF-1.4").unwrap();
    std::fs::write(project.path().join("notes.txt"), b"words").unwrap();
    std::fs::create_dir(project.path().join("dir.pdf")).unwrap();
    let fifo = std::ffi::CString::new(project.path().join("pipe.pdf").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let dir = tempfile::tempdir().unwrap();
    let ws = row_with_kernel(dir.path(), project.path(), "ws-nopage", "claude", drawn_page(2));
    for (node, page) in [("files:notes.txt", 2), ("files:pipe.pdf", 2), ("files:dir.pdf", 2), ("files:gone.pdf", 2), ("files:doc.PDF", 0)] {
        assert_eq!(page_png(&ws, node, page).await.unwrap_err().0, NOT_AN_IMAGE, "{node} page {page}");
        assert!(!dir.path().join("asked").exists(), "{node} page {page}: the kernel was asked");
    }
    // the same kernel draws the page of the file itself, whatever the case of its extension, and of a file whose whole
    // name is `.pdf`, which the plugin claims by the end of its path
    assert!(page_png(&ws, "files:doc.PDF", 2).await.is_ok());
    assert!(dir.path().join("asked").exists());
    std::fs::create_dir(project.path().join("sub")).unwrap();
    std::fs::write(project.path().join("sub/.pdf"), b"%PDF-1.4").unwrap();
    assert!(page_png(&ws, "files:sub/.pdf", 2).await.is_ok(), "a file named `.pdf` is a PDF to its plugin");
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
        let refused = compose(&files, "r1", &stroke(InkBase::Canvas), None).unwrap_err();
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
        let base = InkBase::Figure { node_id: "files:pipe.png".into(), page: None };
        let composed = compose(&files, "r1", &stroke(base), None).unwrap_err().0;
        let cropped = crop_to_captures(&files, &fifo, "pipe", (0, 0, 1, 1)).unwrap_err().0;
        tx.send((composed, cropped)).ok();
    });
    let got = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("a FIFO held the call");
    assert_eq!((got.0, got.1.as_str()), (NOT_AN_IMAGE, "decode_failed"));
}
