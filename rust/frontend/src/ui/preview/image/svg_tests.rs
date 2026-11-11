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

/// `f` on a thread with the stack of the Linux main thread, where the window parses: a parser that recurses once per
/// element overflows it on the inputs below.
fn on_a_main_sized_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(8 << 20).spawn(f).expect("spawn").join().expect("the parse panicked")
}

fn nested(open: &str, close: &str, depth: usize) -> String {
    format!("<svg {SVG_NS} width=\"10\" height=\"10\">{}{}</svg>", open.repeat(depth), close.repeat(depth))
}

/// A document nested past usvg's own limit is refused before roxmltree recurses into it: 20000 levels, written so a
/// scanner that counts `/>` inside an attribute value as a close sees none, through an entity
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
    for (name, svg) in [("plain", plain), ("quoted", quoted), ("entity", entity), ("element", element), ("pi", pi)] {
        let got = on_a_main_sized_stack(move || svg_tree(svg.as_bytes()).map(|_| ()).map_err(|why| format!("{why:#}")));
        match (name, got) {
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
    // The other shapes a DOCTYPE takes: no external ID, either quote, spaces before the close.
    for doctype in [r#"<!DOCTYPE svg>"#, r#"<!DOCTYPE svg SYSTEM "a.dtd">"#, "<!DOCTYPE svg [ <!ENTITY a 'b' > <!-- c --> ]  >"] {
        svg_tree(format!(r#"{doctype}<svg {SVG_NS} width="10" height="10"/>"#).as_bytes()).unwrap_or_else(|why| panic!("{doctype}: {why:#}"));
    }
}

