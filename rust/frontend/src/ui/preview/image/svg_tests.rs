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

const SVG_NS: &str = r#"xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink""#;

fn nested(open: &str, close: &str, depth: usize) -> String {
    format!("<svg {SVG_NS} width=\"10\" height=\"10\">{}{}</svg>", open.repeat(depth), close.repeat(depth))
}

/// A document nested past usvg's own limit is refused before roxmltree recurses into it: 20000 levels, written so a
/// scanner that counts `/>` inside an attribute value as a close sees none, inside an embedded SVG, through an entity
/// that holds markup, and through one that follows a `]>` inside an `<!ELEMENT` or a processing instruction (roxmltree
/// does not end the DOCTYPE there), each end in an error or a refusal and never in a stack overflow. Each runs in a child
/// process, where the unguarded parse aborts the process (a red is the child's abort, which fails
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
    let deep_entity = |decl: &str| {
        format!(
            r#"<!DOCTYPE svg [{decl}<!ENTITY a "{}{}">]><svg {SVG_NS} width="10" height="10">&a;</svg>"#,
            "<g>".repeat(20_000),
            "</g>".repeat(20_000)
        )
    };
    let (element, pi) = (deep_entity("<!ELEMENT x ]>"), deep_entity("<?x ]>?>"));
    for (name, svg) in [("plain", plain), ("quoted", quoted), ("embedded", embedded), ("entity", entity), ("element", element), ("pi", pi)] {
        let got = on_svg_stack(|| svg_tree(svg.as_bytes()).map(|_| ())).map_err(|why| format!("{why:#}"));
        match (name, got) {
            ("embedded", Ok(())) => {} // the nested SVG is refused (`embedded_images_only`), the outer one draws
            (_, Err(why)) => assert!(why.contains("nested") || why.contains("DOCTYPE"), "{name}: {why}"),
            (_, Ok(())) => panic!("{name}: parsed"),
        }
    }
}

/// The limit is usvg's: 1024 elements deep (the root among them) is taken, 1025 refused.
#[test]
fn the_nesting_limit_is_pinned_at_1024() {
    assert!(check_xml_shape(nested("<g>", "</g>", 1023).as_bytes()).is_ok());
    let why = format!("{:#}", check_xml_shape(nested("<g>", "</g>", 1024).as_bytes()).expect_err("1025 deep"));
    assert!(why.contains("nested deeper than 1024"), "{why}");
}

/// A nesting an editor can write still draws, and one past the limit is refused with its reason.
#[test]
fn nesting_within_the_limit_draws_and_past_it_is_refused() {
    let ok = on_svg_stack(|| svg_tree(nested("<g>", "</g>", 500).as_bytes()).map(|_| ())).map_err(|why| format!("{why:#}"));
    assert_eq!(ok, Ok(()));
    let deep = nested("<g>", "</g>", 1100);
    let why = format!("{:#}", on_svg_stack(|| svg_tree(deep.as_bytes()).map(|_| ())).expect_err("1100 levels"));
    assert!(why.contains("nested deeper than 1024"), "{why}");
    // An Illustrator-style DOCTYPE entity of a URL is no markup and parses; comments, CDATA and a PI hold no element.
    let doc = format!(
        r#"<?xml version="1.0"?><!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [<!ENTITY ns "http://ns.adobe.com/Extensibility/1.0/">]><!-- <g><g> --><svg {SVG_NS} width="10" height="10"><![CDATA[<g><g>]]><g a="x>y"/></svg>"#
    );
    svg_tree(doc.as_bytes()).expect("a plain document parses");
    // The other shapes a DOCTYPE takes: no external ID, either quote, spaces before the close.
    for doctype in [r#"<!DOCTYPE svg>"#, r#"<!DOCTYPE svg SYSTEM "a.dtd">"#, "<!DOCTYPE svg [ <!ENTITY a 'b' > <!-- c --> ]  >"] {
        svg_tree(format!(r#"{doctype}<svg {SVG_NS} width="10" height="10"/>"#).as_bytes()).unwrap_or_else(|why| panic!("{doctype}: {why:#}"));
    }
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

/// A file named by an `<image>` href is not read: nothing is drawn.
#[test]
fn an_image_href_naming_a_local_file_draws_nothing() {
    let dir = Scratch::new("href-file");
    let local = dir.0.join("local.png");
    std::fs::write(&local, green_png()).expect("write a PNG");
    assert_eq!(center(image_svg(&local.to_string_lossy())), [0, 0, 0, 0], "a file named by an href was drawn");
}

/// A FIFO named by an `<image>` href is not opened: the parse returns at once and draws nothing.
#[cfg(unix)]
#[test]
fn an_image_href_naming_a_fifo_returns_at_once() {
    use std::os::unix::fs::FileTypeExt;
    let dir = Scratch::new("href-fifo");
    let fifo = dir.0.join("fifo");
    sot_platform::test_wait::run_within(std::process::Command::new("mkfifo").arg(&fifo), std::time::Duration::from_secs(10)).ok();
    assert!(std::fs::metadata(&fifo).expect("the FIFO exists").file_type().is_fifo(), "mkfifo made no FIFO");
    let href = fifo.to_string_lossy().into_owned();
    let drawn = sot_platform::test_wait::within(std::time::Duration::from_secs(10), "an SVG whose image href is a FIFO", move || center(image_svg(&href)));
    assert_eq!(drawn, [0, 0, 0, 0]);
}

/// One embedded raster repeated by `<use>` is bounded in all, not only each: usvg asks the resolver for each use and
/// resvg decodes each, so 100 uses of a 1 Mpx image draw while the document's `MAX_IMAGE_PIXELS` last and no longer.
#[test]
fn an_embedded_raster_repeated_past_the_pixel_budget_stops_drawing() {
    use base64::Engine;
    let mut png = Vec::new();
    image::RgbaImage::from_pixel(1024, 1024, image::Rgba([0, 255, 0, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode a PNG");
    let data = base64::engine::general_purpose::STANDARD.encode(&png);
    let uses: String = (0..100).map(|k| format!(r##"<use xlink:href="#i" x="{k}"/>"##)).collect();
    let svg = format!(
        r#"<svg {SVG_NS} width="100" height="1"><defs><image id="i" width="1" height="1" xlink:href="data:image/png;base64,{data}"/></defs>{uses}</svg>"#
    );
    let tree = svg_tree(svg.as_bytes()).expect("the SVG parses");
    let pixmap = draw(&tree, 100, 1).expect("the SVG draws");
    let drawn = pixmap.data().chunks(4).filter(|px| px[3] != 0).count() as u64;
    assert_eq!(drawn, MAX_IMAGE_PIXELS / (1024 * 1024), "the instances drawn");
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

/// An SVG is parsed and drawn on a thread of `SVG_STACK` whatever the caller's: 1000 levels of nesting need about
/// 1.2 MiB in a release build and five times that in a debug one, so on a 256 KiB caller it overflows unless it is
/// drawn on the SVG's own thread (in a child process, where an overflow aborts it).
#[test]
fn an_svg_draws_on_its_own_stack_whatever_the_callers() {
    if !sot_platform::test_isolated::run_isolated("ui::preview::image::svg::tests::an_svg_draws_on_its_own_stack_whatever_the_callers") {
        return;
    }
    let svg = nested("<g>", "</g>", 1000);
    let drawn = std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(move || on_svg_stack(|| draw(&svg_tree(svg.as_bytes())?, 10, 10).map(|_| ())))
        .expect("spawn")
        .join()
        .expect("the caller's thread");
    drawn.expect("1000 levels draw");
}

/// A panic while drawing is an error for the caller, not a panic in it.
#[test]
fn a_panic_on_the_svg_thread_is_an_error() {
    let why = on_svg_stack::<()>(|| panic!("boom")).expect_err("a panic");
    assert!(format!("{why:#}").contains("panicked"), "{why:#}");
}
/// A chain of `n` links of `kind` (`clip-path`, `mask` or a `pattern` fill), each link its own element at depth 3, and
/// a rectangle that starts it.
fn chain(kind: &str, n: usize) -> String {
    let link = |k: usize| if k + 1 < n { format!(r#" {}="url(#c{})""#, if kind == "pattern" { "fill" } else { kind }, k + 1) } else { String::new() };
    let body: String = (0..n)
        .map(|k| match kind {
            "clip-path" => format!(r#"<clipPath id="c{k}"><rect width="1" height="1"{}/></clipPath>"#, link(k)),
            "mask" => format!(r#"<mask id="c{k}"><rect width="1" height="1" fill="white"{}/></mask>"#, link(k)),
            _ => format!(r#"<pattern id="c{k}" width="1" height="1" patternUnits="userSpaceOnUse"><rect width="1" height="1"{}/></pattern>"#, link(k)),
        })
        .collect();
    let start = if kind == "pattern" { r#"fill="url(#c0)""#.to_string() } else { format!(r#"{kind}="url(#c0)""#) };
    format!(r#"<svg {SVG_NS} width="10" height="10"><defs>{body}</defs><rect width="10" height="10" {start}/></svg>"#)
}

/// The recursion cost of `svg`, or the refusal.
fn cost_of(svg: &str) -> Result<()> {
    on_svg_stack(|| {
        let doc = roxmltree::Document::parse_with_options(svg, ParsingOptions { allow_dtd: true, ..ParsingOptions::default() })?;
        check_reference_cost(&doc)
    })
}

/// A chain of references nests the converter and the renderer with no XML nesting, so 20000 links of `clip-path`, of
/// `mask` and of a `pattern` fill are refused, each in a child process where the unbounded draw aborts it (as the
/// nesting test does), and a short chain still draws.
#[test]
fn a_long_chain_of_references_is_refused_and_a_short_one_draws() {
    if !sot_platform::test_isolated::run_isolated("ui::preview::image::svg::tests::a_long_chain_of_references_is_refused_and_a_short_one_draws") {
        return;
    }
    for kind in ["clip-path", "mask", "pattern"] {
        let long = chain(kind, 20_000);
        let why = format!("{:#}", on_svg_stack(|| svg_tree(long.as_bytes()).map(|_| ())).expect_err(kind));
        assert!(why.contains("nests") && why.contains("past 4096"), "{kind}: {why}");
        let short = chain(kind, 10);
        on_svg_stack(|| svg_tree(short.as_bytes()).map(|_| ())).unwrap_or_else(|why| panic!("{kind}: {why:#}"));
    }
}

/// The bound is pinned where the cost model puts it: a link costs the depth of the element that holds it below its
/// owner plus `REF_COST`, so a `clip-path` link (its rectangle one below the `clipPath`) costs 5, the chain 5n + 2, and
/// 4096 units are 818 links.
#[test]
fn the_reference_bound_is_pinned_at_4096_units() {
    assert!(cost_of(&chain("clip-path", 818)).is_ok());
    let why = format!("{:#}", cost_of(&chain("clip-path", 819)).expect_err("819 links"));
    assert!(why.contains("past 4096"), "{why}");
    // Nesting alone costs a unit a level, up to usvg's 1024.
    assert!(cost_of(&nested("<g>", "</g>", 1023)).is_ok());
}

/// What a real file holds draws: a cycle of references (usvg breaks it), a stylesheet whose classes fill with 200
/// gradients, and `<use>` of one symbol 3000 times, each costing its depth and no more; a stylesheet that chains
/// 3000 clip paths does not.
#[test]
fn cycles_stylesheets_and_many_uses_are_not_taken_for_deep_chains() {
    let cycle = format!(
        r##"<svg {SVG_NS} width="10" height="10"><defs><clipPath id="a"><rect width="1" height="1" clip-path="url(#b)"/></clipPath><clipPath id="b"><rect width="1" height="1" clip-path="url(#a)"/></clipPath></defs><rect width="10" height="10" clip-path="url(#a)"/></svg>"##
    );
    on_svg_stack(|| svg_tree(cycle.as_bytes()).map(|_| ())).expect("a cycle of clip paths");
    let gradients: String = (0..200).map(|k| format!(r##"<linearGradient id="g{k}"><stop offset="0"/></linearGradient>"##)).collect();
    let classes: String = (0..200).map(|k| format!(".s{k}{{fill:url(#g{k})}}")).collect();
    let rects: String = (0..200).map(|k| format!(r#"<rect class="s{k}" width="1" height="1"/>"#)).collect();
    let sheet = format!(r#"<svg {SVG_NS} width="10" height="10"><style>{classes}</style><defs>{gradients}</defs>{rects}</svg>"#);
    cost_of(&sheet).expect("a stylesheet of gradients");
    // The same through `clip-path` classes is a chain a stylesheet can build, and is refused.
    let clips: String = (0..3000).map(|k| format!(r#"<clipPath id="c{k}" class="s{k}"><rect width="1" height="1"/></clipPath>"#)).collect();
    let rules: String = (0..3000).map(|k| format!(".s{k}{{clip-path:url(#c{})}}", k + 1)).collect();
    let chained = format!(r#"<svg {SVG_NS} width="10" height="10"><style>{rules}</style><defs>{clips}</defs></svg>"#);
    assert!(cost_of(&chained).is_err(), "a stylesheet's chain of 3000 clip paths");
    let uses: String = (0..3000).map(|_| r##"<use xlink:href="#s"/>"##).collect();
    let many = format!(r##"<svg {SVG_NS} width="10" height="10"><defs><g id="s"><rect width="1" height="1" clip-path="url(#c)"/></g><clipPath id="c"><rect width="1" height="1"/></clipPath></defs>{uses}</svg>"##);
    cost_of(&many).expect("many uses");
}

