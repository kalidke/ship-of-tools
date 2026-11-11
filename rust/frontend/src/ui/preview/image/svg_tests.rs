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

const RED_BOX: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#ff0000"/></svg>"##;

/// The picture fills the pane on its limiting side, keeps the SVG's aspect ratio and never passes the texture limit.
#[test]
fn the_svg_raster_fits_the_pane_keeps_its_aspect_and_stays_under_the_texture_limit() {
    let fit = |pane, cap| svg_fit_dims((200.0, 100.0), pane, cap);
    assert_eq!(fit(Some((800, 800)), 16384), (800, 400), "width limits");
    assert_eq!(fit(Some((600, 100)), 16384), (200, 100), "height limits");
    assert_eq!(fit(Some((100, 800)), 16384), (100, 50));
    assert_eq!(fit(Some((20000, 20000)), 4096), (4096, 2048), "the texture limit wins, aspect kept");
    assert_eq!(fit(None, 16384), (1024, 512), "no pane yet: a small fixed bound, not the SVG's own size");
    assert_eq!(svg_fit_dims((1.0, 1000.0), Some((50, 50)), 16384), (1, 50), "a side is never under one pixel");
}

/// The raster's area is bounded as well as each side: an SVG that declares a huge size draws at the fixed bound with no
/// pane, and no pane or texture limit lifts a picture past the pixel budget.
#[test]
fn the_svg_raster_is_bounded_by_area_and_a_huge_declared_size_draws_small() {
    let area = |(w, h): (u32, u32)| u64::from(w) * u64::from(h);
    assert!(area(svg_fit_dims((1e30, 1e30), None, 16384)) <= 1024 * 1024);
    assert!(area(svg_fit_dims((100.0, 100.0), Some((60000, 60000)), 65535)) <= MAX_RASTER_PIXELS);
    assert!(area(svg_fit_dims((100.0, 25.0), Some((30000, 60000)), 65535)) <= MAX_RASTER_PIXELS);
}

/// The raster is the SVG drawn at that size: its pixels are as many as it says, and the drawing fills them.
#[test]
fn the_svg_raster_is_drawn_at_the_fit_size() {
    let (rgba, w, h) = rasterize_fit(RED_BOX, Some((400, 400)), 16384).expect("the SVG draws");
    assert_eq!((w, h), (400, 200));
    assert_eq!(rgba.len(), 400 * 200 * 4);
    let px = |x: usize, y: usize| &rgba[(y * 400 + x) * 4..][..4];
    assert_eq!(px(200, 100), [255, 0, 0, 255]);
    assert_eq!(px(0, 0), [255, 0, 0, 255]);
    assert_eq!(px(399, 199), [255, 0, 0, 255]);
}

/// An SVG that will not parse is an error with its reason, and the status line says it was not drawn.
#[test]
fn an_svg_that_will_not_parse_says_why_for_the_status_line() {
    let why = rasterize_fit(b"<svg><not closed", Some((400, 400)), 16384).expect_err("not an SVG");
    let line = svg_not_drawn(&why);
    assert!(line.starts_with("svg not drawn: "), "{line}");
    assert!(line.contains("svg parse failed"), "{line}");
    assert!(!line.contains('\n'), "one line: {line}");
    // Only the error's first line, and no control character of the file's own bytes, reaches the line.
    let long = anyhow::anyhow!("first \u{7}line\nsecond line");
    assert_eq!(svg_not_drawn(&long), "svg not drawn: first line");
}

const SVG_NS: &str = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#;

/// `f` on a thread with the stack of the Linux main thread, where the window parses: a parser that recurses once per
/// element overflows it on the inputs below.
fn on_a_main_sized_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(8 << 20).spawn(f).expect("spawn").join().expect("the parse panicked")
}

fn nested(open: &str, close: &str, depth: usize) -> String {
    format!("<svg {SVG_NS} width=\"10\" height=\"10\">{}{}</svg>", open.repeat(depth), close.repeat(depth))
}

/// A document nested past usvg's own limit is refused before roxmltree recurses into it: 20000 levels, written so a
/// scanner that counts `/>` inside an attribute value as a close sees none, inside an embedded SVG, and through an
/// entity that holds markup, each end in an error or a refusal and never in a stack overflow. Each runs in a child
/// process on an 8 MiB stack, where the unguarded parse aborts the process (a red is the child's abort, which fails
/// this test, not the binary).
#[test]
fn a_deeply_nested_svg_is_refused_before_the_parser_recurses_into_it() {
    if !sot_platform::test_isolated::run_isolated("ui::preview::image::svg::tests::a_deeply_nested_svg_is_refused_before_the_parser_recurses_into_it") {
        return;
    }
    let plain = nested("<g>", "</g>", 20_000);
    let quoted = nested(r#"<g a="/>">"#, "</g>", 20_000);
    let embedded = {
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(&plain);
        format!(r#"<svg {SVG_NS} width="10" height="10"><image width="10" height="10" xlink:href="data:image/svg+xml;base64,{data}"/></svg>"#)
    };
    let entity = format!(
        r#"<!DOCTYPE svg [<!ENTITY a "{}{}">]><svg {SVG_NS} width="10" height="10">&a;</svg>"#,
        "<g>".repeat(100),
        "</g>".repeat(100)
    );
    for (name, svg) in [("plain", plain), ("quoted", quoted), ("embedded", embedded), ("entity", entity)] {
        let got = on_a_main_sized_stack(move || svg_tree(svg.as_bytes()).map(|_| ()).map_err(|why| format!("{why:#}")));
        match (name, got) {
            ("embedded", Ok(())) => {} // the nested SVG is refused (`embedded_images_only`), the outer one draws
            (_, Err(why)) => assert!(why.contains("nested") || why.contains("entity"), "{name}: {why}"),
            (_, Ok(())) => panic!("{name}: parsed"),
        }
    }
}

/// A nesting an editor can write still draws, and one past the limit is refused with its reason.
#[test]
fn nesting_within_the_limit_draws_and_past_it_is_refused() {
    let ok = on_a_main_sized_stack(|| svg_tree(nested("<g>", "</g>", 500).as_bytes()).map(|_| ()).map_err(|why| format!("{why:#}")));
    assert_eq!(ok, Ok(()));
    let deep = nested("<g>", "</g>", 1100);
    let why = on_a_main_sized_stack(move || svg_tree(deep.as_bytes()).err().map(|why| format!("{why:#}")).unwrap_or_default());
    assert!(why.contains("nested deeper than 1024"), "{why}");
    // An Illustrator-style DOCTYPE entity of a URL is no markup and parses; comments, CDATA and a PI hold no element.
    let doc = format!(
        r#"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [<!ENTITY ns "http://ns.adobe.com/Extensibility/1.0/">]><!-- <g><g> --><svg {SVG_NS} width="10" height="10"><![CDATA[<g><g>]]><g a="x>y"/></svg>"#
    );
    svg_tree(doc.as_bytes()).expect("a plain document parses");
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
    let pixmap = draw(&svg_tree(svg.as_bytes()).expect("the SVG parses"), 100, 100).expect("the SVG draws");
    pixmap.data()[(50 * 100 + 50) * 4..][..4].try_into().expect("a pixel")
}

fn image_svg(href: &str) -> String {
    format!(r#"<svg {SVG_NS} width="10" height="10"><image width="10" height="10" xlink:href="{href}"/></svg>"#)
}

/// A file named by an `<image>` href is not read: nothing is drawn.
#[test]
fn an_image_href_naming_a_local_file_draws_nothing() {
    let dir = std::env::temp_dir().join(format!("sot-svg-href-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp folder");
    let local = dir.join("local.png");
    std::fs::write(&local, green_png()).expect("write a PNG");
    assert_eq!(center(image_svg(&local.to_string_lossy())), [0, 0, 0, 0], "a file named by an href was drawn");
    std::fs::remove_dir_all(&dir).expect("remove the temp folder");
}

/// A FIFO named by an `<image>` href is not opened: the parse returns at once and draws nothing.
#[cfg(unix)]
#[test]
fn an_image_href_naming_a_fifo_returns_at_once() {
    let dir = std::env::temp_dir().join(format!("sot-svg-href-fifo-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp folder");
    let fifo = dir.join("fifo");
    sot_platform::test_wait::run_within(std::process::Command::new("mkfifo").arg(&fifo), std::time::Duration::from_secs(10)).ok();
    let href = fifo.to_string_lossy().into_owned();
    let drawn = sot_platform::test_wait::within(std::time::Duration::from_secs(10), "an SVG whose image href is a FIFO", move || center(image_svg(&href)));
    assert_eq!(drawn, [0, 0, 0, 0]);
    std::fs::remove_dir_all(&dir).expect("remove the temp folder");
}

/// An `<image>` draws what its own data holds: an embedded PNG draws, and nested SVG data is refused.
#[test]
fn an_embedded_raster_draws_and_embedded_svg_does_not() {
    use base64::Engine;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    assert_eq!(center(image_svg(&format!("data:image/png;base64,{}", b64(&green_png())))), [0, 255, 0, 255], "an embedded PNG");
    let red = format!(r##"<svg {SVG_NS} width="4" height="4"><rect width="4" height="4" fill="#ff0000"/></svg>"##);
    assert_eq!(center(image_svg(&format!("data:image/svg+xml;base64,{}", b64(red.as_bytes())))), [0, 0, 0, 0], "nested SVG data was drawn");
}
