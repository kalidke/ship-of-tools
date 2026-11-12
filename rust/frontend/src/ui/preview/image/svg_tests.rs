use super::*;

/// The glyphs usvg laid out for every text node under `group`.
fn glyphs(group: &resvg::usvg::Group) -> usize {
    group
        .children()
        .iter()
        .map(|node| match node {
            resvg::usvg::Node::Text(text) => text.layouted().iter().map(|span| span.positioned_glyphs.len()).sum(),
            resvg::usvg::Node::Group(inner) => glyphs(inner),
            _ => 0,
        })
        .sum()
}

/// A reference is read by its digit run alone, so the scan is linear: a megabyte of `&#` before one `;` (reading to
/// that far `;` from each `&#` made it quadratic, seconds on the window's thread) ends well within the bound, and a
/// sign before the digits is no reference.
#[test]
fn separator_references_are_read_in_one_pass() {
    let long = format!("{};", "&#".repeat(512 * 1024));
    let unchanged = sot_platform::test_wait::within(std::time::Duration::from_secs(10), "a 1 MiB reference scan", move || {
        *separator_references_as_spaces(&long) == *long
    });
    assert!(unchanged, "a run of `&#` with no separator in it changed");
    assert_eq!(separator_references_as_spaces("AA&#x+2029;BB&#x2029;CC&#8233;DD&#x2029"), "AA&#x+2029;BB CC DD&#x2029");
}

/// `svg` as gzip, what an `.svgz` holds.
fn gzip(svg: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(svg).expect("gzip in memory");
    gz.finish().expect("gzip in memory")
}

/// An SVG gzipped twice is refused: unpacking one layer would leave usvg to unpack the second past the door, and
/// its text after a paragraph separator would be lost.
#[test]
fn a_twice_gzipped_svgz_is_refused() {
    let family = system_fontdb()
        .faces()
        .find_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .expect("a system font");
    let text = "AA\u{2029}BB";
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="50"><text x="0" y="30" font-family="{family}" font-size="20">{text}</text></svg>"#
    );
    match svg_tree(&gzip(&gzip(svg.as_bytes()))) {
        Ok(tree) => panic!("a twice-gzipped svgz parsed, {} glyphs", glyphs(tree.root())),
        Err(why) => assert!(format!("{why:#}").contains("svgz holds a second gzip layer"), "{why:#}"),
    }
}

/// An `.svgz` is unpacked to its cap and no further: a small gzip of 4 KiB of spaces is refused under a 1 KiB cap,
/// with the reason, and unpacks whole under a 4 KiB one.
#[test]
fn an_svgz_past_its_cap_is_refused() {
    let packed = gzip(&[b' '; 4096]);
    assert!(packed.len() < 1024, "the test's gzip is not small: {} bytes", packed.len());
    let refused = unpack_svgz(&packed, 1024).expect_err("an svgz past its cap");
    assert!(format!("{refused:#}").contains("svgz unpacks past its cap of 1024 bytes"), "{refused:#}");
    assert_eq!(unpack_svgz(&packed, 4096).expect("an svgz at its cap").len(), 4096);
}

/// An `.svgz` is unpacked before the door, not by usvg after it, so its text after a paragraph separator is drawn
/// too.
#[test]
fn svgz_text_after_a_paragraph_separator_is_drawn() {
    let family = system_fontdb()
        .faces()
        .find_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .expect("a system font");
    let drawn = |text: &str, packed: bool| {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="50"><text x="0" y="30" font-family="{family}" font-size="20">{text}</text></svg>"#
        );
        let bytes = if packed { gzip(svg.as_bytes()) } else { svg.into_bytes() };
        glyphs(svg_tree(&bytes).expect("the SVG parses").root())
    };
    let whole = drawn("AA BB", false);
    assert!(whole > 0 && drawn("AA BB", true) == whole, "no text drawn in {family}, or the svgz differs");
    let cases = ["AA\u{2029}BB", "AA&#x2029;BB"];
    let got: Vec<(&str, usize)> = cases.iter().map(|&text| (text, drawn(text, true))).collect();
    assert_eq!(got, cases.map(|text| (text, whole)));
}

/// Monospace SVG text stays in Consolas where the machine holds it, and otherwise takes a monospaced family the
/// machine holds.
#[test]
fn monospace_stays_consolas_where_it_is_held() {
    let face = |family: &str| fontdb::FaceInfo {
        id: fontdb::ID::dummy(),
        source: fontdb::Source::Binary(Arc::new(Vec::<u8>::new())),
        index: 0,
        families: vec![(family.to_string(), fontdb::Language::English_UnitedStates)],
        post_script_name: family.to_string(),
        style: fontdb::Style::Normal,
        weight: fontdb::Weight::NORMAL,
        stretch: fontdb::Stretch::Normal,
        monospaced: true,
    };
    let monospace = |families: &[&str]| {
        let mut db = fontdb::Database::new();
        for family in families {
            db.push_face_info(face(family));
        }
        held_generic_families(&mut db);
        db.family_name(&fontdb::Family::Monospace).to_string()
    };
    assert_eq!(monospace(&["DejaVu Sans Mono", "Consolas"]), "Consolas");
    assert_eq!(monospace(&["DejaVu Sans Mono"]), "DejaVu Sans Mono");
}

/// An SVG's text in each generic family is drawn, on every platform: each names a family this machine holds.
#[test]
fn svg_text_in_each_generic_family_is_drawn() {
    let drawn: Vec<(&str, usize)> = ["serif", "sans-serif", "monospace"]
        .into_iter()
        .map(|family| {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="50"><text x="0" y="30" font-family="{family}" font-size="20">AA BB</text></svg>"#
            );
            (family, glyphs(svg_tree(svg.as_bytes()).expect("the SVG parses").root()))
        })
        .collect();
    assert!(drawn.iter().all(|&(_, n)| n > 0), "{drawn:?}");
}

/// usvg shapes only a text chunk's first bidi paragraph: the text after a U+2029 or a U+0085, written as itself or
/// as a character reference, is drawn, as the same text with a space there is.
#[test]
fn svg_text_after_a_paragraph_separator_is_drawn() {
    // A family the system has, so text is drawn on every platform.
    let family = system_fontdb()
        .faces()
        .find_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .expect("a system font");
    let drawn = |text: &str| {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="50"><text x="0" y="30" font-family="{family}" font-size="20">{text}</text></svg>"#
        );
        glyphs(svg_tree(svg.as_bytes()).expect("the SVG parses").root())
    };
    let whole = drawn("AA BB");
    assert!(whole > 0, "no text drawn in {family}");
    let cases = ["AA\u{2029}BB", "AA&#x2029;BB", "AA&#8233;BB", "AA\u{85}BB", "AA&#133;BB"];
    let got: Vec<(&str, usize)> = cases.iter().map(|&text| (text, drawn(text))).collect();
    assert_eq!(got, cases.map(|text| (text, whole)));
}

/// A 4x4 green PNG.
fn green_png() -> Vec<u8> {
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 255, 0, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode a PNG");
    png
}

/// The center pixel of `svg` drawn at 100x100.
fn center(svg: String) -> [u8; 4] {
    let tree = svg_tree(svg.as_bytes()).expect("the SVG parses");
    let mut pixmap = Pixmap::new(100, 100).expect("a pixmap");
    let size = tree.size();
    resvg::render(&tree, Transform::from_scale(100.0 / size.width(), 100.0 / size.height()), &mut pixmap.as_mut());
    pixmap.data()[(50 * 100 + 50) * 4..][..4].try_into().expect("a pixel")
}

fn image_svg(href: &str) -> String {
    format!(r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="10" height="10"><image width="10" height="10" xlink:href="{href}"/></svg>"#)
}

/// A temporary folder removed when it drops, so a failing test leaves nothing behind.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sot-svg-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp folder");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A file named by an `<image>` href, plainly or as `file://`, is not read: nothing is drawn.
#[test]
fn an_image_href_naming_a_local_file_draws_nothing() {
    let dir = Scratch::new("href-file");
    let local = dir.0.join("local.png");
    std::fs::write(&local, green_png()).expect("write a PNG");
    let path = local.to_string_lossy().into_owned();
    assert_eq!(center(image_svg(&path)), [0, 0, 0, 0], "a file named by an href was drawn");
    assert_eq!(center(image_svg(&format!("file://{path}"))), [0, 0, 0, 0], "a file:// href was drawn");
}

/// A FIFO named by an href is not opened, whichever element carries the href (`<image>`, `<feImage>`, a `<pattern>`'s
/// image, a `<mask>`'s image) and however the path is spelled (absolute, relative to the working folder, `file://`):
/// each draw returns within its bound. Red on usvg's default resolver, which opens the FIFO and waits for a writer.
#[cfg(unix)]
#[test]
fn an_href_naming_a_fifo_draws_nothing_through_every_element_and_spelling() {
    use std::os::unix::fs::FileTypeExt;
    let dir = Scratch::new("href-fifo");
    let fifo = dir.0.join("fifo");
    sot_platform::test_wait::run_within(std::process::Command::new("mkfifo").arg(&fifo), std::time::Duration::from_secs(10)).ok();
    assert!(std::fs::metadata(&fifo).expect("the FIFO exists").file_type().is_fifo(), "mkfifo made no FIFO");
    // The fonts load once, before any bound starts.
    let _ = system_fontdb();
    let absolute = fifo.to_string_lossy().into_owned();
    let relative = format!("{}{}", "../".repeat(64), absolute.trim_start_matches('/'));
    let spellings = [absolute.clone(), relative, format!("file://{absolute}")];
    let ns = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#;
    let elements: [(&str, fn(&str) -> String); 4] = [
        ("image", |h| format!(r#"<image width="10" height="10" xlink:href="{h}"/>"#)),
        ("feImage", |h| format!(r#"<filter id="f"><feImage xlink:href="{h}"/></filter><rect width="10" height="10" filter="url(#f)"/>"#)),
        ("pattern", |h| {
            format!(r##"<pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><image width="10" height="10" xlink:href="{h}"/></pattern><rect width="10" height="10" fill="url(#p)"/>"##)
        }),
        ("mask", |h| {
            format!(r##"<mask id="m"><image width="10" height="10" xlink:href="{h}"/></mask><rect width="10" height="10" fill="red" mask="url(#m)"/>"##)
        }),
    ];
    let mut pending = Vec::new();
    for (element, body) in elements {
        for href in &spellings {
            let svg = format!(r#"<svg {ns} width="10" height="10">{}</svg>"#, body(href));
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(center(svg));
            });
            pending.push((format!("{element} {href}"), rx));
        }
    }
    for (what, rx) in pending {
        let drawn = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap_or_else(|_| panic!("{what}: the draw did not return: the FIFO was opened"));
        assert_eq!(drawn, [0, 0, 0, 0], "{what}");
    }
}

/// The control: an embedded PNG still draws.
#[test]
fn an_embedded_png_image_still_draws() {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(green_png());
    assert_eq!(center(image_svg(&format!("data:image/png;base64,{data}"))), [0, 255, 0, 255]);
}

/// The picture fills the pane on its limiting side, keeps the SVG's aspect ratio and never passes the texture limit.
#[test]
fn the_svg_raster_fits_the_pane_keeps_its_aspect_and_stays_under_the_texture_limit() {
    let fit = |pane, cap| svg_fit_dims((200.0, 100.0), pane, cap);
    assert_eq!(fit((800, 800), 16384), (800, 400), "width limits");
    assert_eq!(fit((600, 100), 16384), (200, 100), "height limits");
    assert_eq!(fit((100, 800), 16384), (100, 50));
    assert_eq!(fit((20000, 20000), 4096), (4096, 2048), "the texture limit wins, aspect kept");
    assert_eq!(fit((1024, 1024), 16384), (1024, 512), "no pane yet: a small fixed bound, not the SVG's own size");
    assert_eq!(svg_fit_dims((1.0, 1000.0), (50, 50), 16384), (1, 50), "a side is never under one pixel");
}

/// The raster's area is bounded as well as each side: an SVG that declares a huge size draws at the fixed bound with no
/// pane, and no pane or texture limit lifts a picture past the pixel budget.
#[test]
fn the_svg_raster_is_bounded_by_area_and_a_huge_declared_size_draws_small() {
    let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
    assert!(area(svg_fit_dims((1e30, 1e30), (1024, 1024), 16384)) <= 1024 * 1024);
    assert!(area(svg_fit_dims((100.0, 100.0), (60000, 60000), 65535)) <= MAX_RASTER_PIXELS);
    assert!(area(svg_fit_dims((100.0, 25.0), (30000, 60000), 65535)) <= MAX_RASTER_PIXELS);
}
