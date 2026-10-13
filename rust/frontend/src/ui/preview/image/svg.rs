// ui/preview/image/svg.rs — SVG bytes → RGBA8 bitmap via resvg → quad.
//
// This is the path inline-math takes: the backend's MathJax sidecar produces
// SVG for each math snippet (per ADR 0012); we rasterise here at the size the
// preview-layer assigns and upload as a quad. resvg supports MathJax-shape
// SVG natively; complex CSS-styled SVGs may need tweaks later.
//
// Render scale is keyed off the target rect's pixel size so math stays crisp
// at HiDPI without us tracking DPR explicitly — the caller passes the target
// pixel dimensions, we rasterise to those.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{fontdb, Options, Tree};

use crate::ui::render::quad::{Quad, QuadPipeline};
use crate::ui::render::text::{one_paragraph_per_line, PARAGRAPH_SEPARATORS};

/// System-font database, populated once on first SVG. usvg's default fontdb
/// is empty, so `font-family="serif"` would fall back to nothing and drop the
/// text's glyphs; `load_system_fonts()` fills it, and `held_generic_families`
/// points each generic family at a font it holds.
fn system_fontdb() -> Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        held_generic_families(&mut db);
        tracing::info!(count = db.len(), "usvg system fontdb loaded");
        Arc::new(db)
    })
    .clone()
}

/// Points each generic family (`serif`, `sans-serif`, `monospace`, the families MathJax's text and the Monitor's
/// labels ask for) at a family `db` holds, so text in one is drawn. `load_system_fonts` has named them: fontconfig's
/// choice on Linux, else fontdb's Times New Roman, Arial and Courier New, which Windows and macOS ship. A name `db` does
/// not hold (fontconfig's first preference missing from a lean Linux) becomes the family of the first face it does
/// hold, a monospaced one for `monospace`. A machine that holds Consolas keeps it for `monospace`: Windows drew
/// monospace SVG text in it (the Monitor's labels among it) before these defaults, and fontdb's is Courier New there.
fn held_generic_families(db: &mut fontdb::Database) {
    use fontdb::Family::{Monospace, SansSerif, Serif};
    fn holds(db: &fontdb::Database, name: &str) -> bool {
        db.faces().any(|face| face.families.iter().any(|(family, _)| family == name))
    }
    if holds(db, "Consolas") {
        db.set_monospace_family("Consolas");
    }
    for generic in [Serif, SansSerif, Monospace] {
        if holds(db, db.family_name(&generic)) {
            continue;
        }
        let monospaced = matches!(generic, Monospace);
        let held = db
            .faces()
            .find(|face| face.monospaced || !monospaced)
            .and_then(|face| face.families.first())
            .map(|(family, _)| family.clone());
        match (generic, held) {
            (Serif, Some(family)) => db.set_serif_family(family),
            (SansSerif, Some(family)) => db.set_sans_serif_family(family),
            (Monospace, Some(family)) => db.set_monospace_family(family),
            _ => {}
        }
    }
}

/// Rasterise SVG bytes into an RGBA8 bitmap of exactly `(target_w,
/// target_h)` and upload as a quad. Non-uniform scaling: the caller is
/// expected to have computed `target_*` from the SVG's intrinsic ex-unit
/// dimensions (see `ui/preview/markdown/media.rs::parse_math_svg_dims` /
/// `MATHJAX_EX_FACTOR`), so the aspect ratio is already correct. If the
/// caller passes an aspect that disagrees with the SVG, the output will
/// stretch — that's a caller bug, not a fit-recovery concern. Doing the
/// scale-to-fit inside this function used to silently turn short
/// equations into letterbox-tall slabs.
pub fn quad_from_svg_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pipeline: &QuadPipeline,
    bytes: &[u8],
    target_w: u32,
    target_h: u32,
) -> Result<Quad> {
    let tree = svg_tree(bytes)?;

    let svg_size = tree.size();
    let sx = target_w as f32 / svg_size.width().max(0.001);
    let sy = target_h as f32 / svg_size.height().max(0.001);

    let pw = target_w.max(1);
    let ph = target_h.max(1);

    let mut pixmap = Pixmap::new(pw, ph)
        .ok_or_else(|| anyhow::anyhow!("pixmap alloc failed for {}x{}", pw, ph))?;

    resvg::render(&tree, Transform::from_scale(sx, sy), &mut pixmap.as_mut());

    Quad::from_rgba8(device, queue, pipeline, pixmap.data(), pw, ph)
}

/// The SVG's tree as the window draws it: system fonts, `currentColor` as white, and its text with no bidi paragraph
/// separator (`one_paragraph_svg`). An `.svgz` (gzip) is unpacked first: usvg would unpack it itself, past both rules.
fn svg_tree(bytes: &[u8]) -> Result<Tree> {
    let opts = Options {
        fontdb: system_fontdb(),
        ..Options::default()
    };
    let unpacked;
    let bytes: &[u8] = if bytes.starts_with(&[0x1f, 0x8b]) {
        unpacked = resvg::usvg::decompress_svgz(bytes).context("svgz unpack failed")?;
        &unpacked
    } else {
        bytes
    };
    // MathJax SVG emits `fill="currentColor" stroke="currentColor"` on
    // its root glyph group, expecting a CSS cascade to resolve the
    // colour. resvg has no cascade, so `currentColor` falls back to
    // black — invisible against our dark chrome bg. Rewrite to white
    // before parsing. `white` is 5 chars vs `currentColor` 12, but
    // SVG is XML-textual and resvg doesn't care about byte offsets.
    let bytes_owned;
    let bytes: &[u8] = if memchr_currentcolor(bytes) {
        bytes_owned = replace_currentcolor_with_white(bytes);
        &bytes_owned
    } else {
        bytes
    };
    #[allow(clippy::disallowed_methods, reason = "text into the shaper: an SVG's one way into usvg, through one_paragraph_svg")]
    let tree = Tree::from_data(&one_paragraph_svg(bytes), &opts);
    tree.context("svg parse failed")
}

/// SVG source whose text holds no bidi paragraph separator: usvg shapes only a text chunk's first bidi paragraph
/// (usvg-0.44.0 src/text/layout.rs:1367-1368) and drops the rest of it, so each separator, written as itself or as a
/// character reference, becomes a space, the rule of the window's shaper door (`one_paragraph_per_line`,
/// render/text.rs). Bytes that are not UTF-8 go on as they are: usvg reads UTF-8 only.
fn one_paragraph_svg(bytes: &[u8]) -> Cow<'_, [u8]> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Cow::Borrowed(bytes);
    };
    let text = separator_references_as_spaces(text);
    match one_paragraph_per_line(&text) {
        Cow::Borrowed(_) if matches!(text, Cow::Borrowed(_)) => Cow::Borrowed(bytes),
        one => Cow::Owned(one.into_owned().into_bytes()),
    }
}

/// `text` with each numeric character reference to one of `PARAGRAPH_SEPARATORS` (`&#x2029;`, `&#8233;`, …) a space.
/// A reference is its digit run and the `;` right after it, so each character is read once (the window's thread runs
/// this before usvg parses a file of any size) and a sign before the digits is no reference.
fn separator_references_as_spaces(text: &str) -> Cow<'_, str> {
    if !text.contains("&#") {
        return Cow::Borrowed(text);
    }
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(at) = rest.find("&#") {
        let tail = &rest[at + 2..];
        let (radix, digits) = match tail.strip_prefix(['x', 'X']) {
            Some(hex) => (16, hex),
            None => (10, tail),
        };
        let end = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
        let after = digits[end..].strip_prefix(';').and_then(|after| {
            let c = u32::from_str_radix(&digits[..end], radix).ok().and_then(char::from_u32)?;
            PARAGRAPH_SEPARATORS.contains(&c).then_some(after)
        });
        match after {
            Some(after) => {
                out.push_str(&rest[..at]);
                out.push(' ');
                rest = after;
            }
            None => {
                out.push_str(&rest[..at + 2]);
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn memchr_currentcolor(bytes: &[u8]) -> bool {
    bytes.windows(12).any(|w| w == b"currentColor")
}

fn replace_currentcolor_with_white(bytes: &[u8]) -> Vec<u8> {
    let s = std::str::from_utf8(bytes).unwrap_or_default();
    s.replace("currentColor", "white").into_bytes()
}

#[cfg(test)]
mod tests {
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
        let unchanged = sot_log::test_wait::within(std::time::Duration::from_secs(10), "a 1 MiB reference scan", move || {
            *separator_references_as_spaces(&long) == *long
        });
        assert!(unchanged, "a run of `&#` with no separator in it changed");
        assert_eq!(separator_references_as_spaces("AA&#x+2029;BB&#x2029;CC&#8233;DD&#x2029"), "AA&#x+2029;BB CC DD&#x2029");
    }

    /// `svg` as gzip, what an `.svgz` holds: one stored deflate block, so no compressor is needed.
    fn gzip(svg: &[u8]) -> Vec<u8> {
        let mut crc = !0u32;
        for &b in svg {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
            }
        }
        let len = u16::try_from(svg.len()).expect("one stored block");
        let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 1];
        out.extend(len.to_le_bytes());
        out.extend((!len).to_le_bytes());
        out.extend(svg);
        out.extend((!crc).to_le_bytes());
        out.extend((svg.len() as u32).to_le_bytes());
        out
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
}
