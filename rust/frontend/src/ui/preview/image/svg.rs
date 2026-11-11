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
use std::io::Read;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{fontdb, Options, Tree};

use sot_protocol::ir::PREVIEW_BINARY_CAP;

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

/// The SVG's tree as the window draws it: system fonts, `currentColor` as white, its text with no bidi paragraph
/// separator (`one_paragraph_svg`) and an XML shape the parser cannot be crashed by (`check_xml_shape`). An `.svgz`
/// (gzip) is unpacked first (`unpack_svgz`): usvg would unpack it itself, past all of it.
fn svg_tree(bytes: &[u8]) -> Result<Tree> {
    let opts = Options {
        fontdb: system_fontdb(),
        ..Options::default()
    };
    let bytes = unpack_svgz(bytes, PREVIEW_BINARY_CAP)?;
    let bytes: &[u8] = &bytes;
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
    let bytes = one_paragraph_svg(bytes);
    check_xml_shape(&bytes)?;
    #[allow(clippy::disallowed_methods, reason = "text into the shaper: an SVG's one way into usvg, through one_paragraph_svg")]
    let tree = Tree::from_data(&bytes, &opts);
    tree.context("svg parse failed")
}

/// usvg's own bound on element nesting, which it checks only after roxmltree has parsed the document, and roxmltree
/// recurses once per element: a 20000-deep file aborts the process first. A scan before the parse loses nothing drawable.
const SVG_MAX_DEPTH: usize = 1024;

/// Refuses XML nested deeper than `SVG_MAX_DEPTH` elements, or whose DOCTYPE is more than roxmltree reads in one:
/// `skip_doctype`. One pass over the bytes, no recursion: a tag ends at the `>` outside its quoted attribute values, and
/// comments, CDATA, processing instructions and the DOCTYPE are skipped whole. A document it passes may still be
/// malformed; the parser says so.
fn check_xml_shape(xml: &[u8]) -> Result<()> {
    let (mut at, mut depth) = (0, 0usize);
    while let Some(lt) = xml[at..].iter().position(|&b| b == b'<') {
        at += lt;
        let rest = &xml[at..];
        if rest.starts_with(b"<!--") {
            at += 4 + find(&rest[4..], b"-->");
        } else if rest.starts_with(b"<![CDATA[") {
            at += 9 + find(&rest[9..], b"]]>");
        } else if rest.starts_with(b"<?") {
            at += 2 + find(&rest[2..], b"?>");
        } else if rest.starts_with(b"<!DOCTYPE") {
            at += skip_doctype(rest)?;
        } else if rest.starts_with(b"</") {
            depth = depth.saturating_sub(1);
            at += 2 + find(&rest[2..], b">") - 1;
        } else {
            let (mut quote, mut i) = (0u8, 1);
            while i < rest.len() {
                match (quote, rest[i]) {
                    (0, q @ (b'"' | b'\'')) => quote = q,
                    (q, b) if q != 0 && b == q => quote = 0,
                    (0, b'>') => break,
                    _ => {}
                }
                i += 1;
            }
            if i > 1 && rest[i - 1] != b'/' {
                depth += 1;
                anyhow::ensure!(depth <= SVG_MAX_DEPTH, "svg is nested deeper than {SVG_MAX_DEPTH} elements");
            }
            at += i + 1;
        }
        if at >= xml.len() {
            break;
        }
    }
    Ok(())
}

/// How far past `needle`'s end it first occurs in `from`, else `from`'s length.
fn find(from: &[u8], needle: &[u8]) -> usize {
    from.windows(needle.len()).position(|w| w == needle).map_or(from.len(), |at| at + needle.len())
}

/// The length of the DOCTYPE at the start of `x`, read as roxmltree reads it (tokenizer.rs `parse_doctype`), and refused
/// where it reads anything else: it ends an `<!ELEMENT`, `<!ATTLIST` or `<!NOTATION` at the first `>` and a processing
/// instruction at `?>`, so a scan that models those loosely leaves the DOCTYPE early and takes the next `<!ENTITY` for
/// an element. Taken: the external ID, then comments and `<!ENTITY name "value">` (either quote) whose value holds no
/// `<`, since roxmltree expands a value as content, then `]` and `>`.
fn skip_doctype(x: &[u8]) -> Result<usize> {
    let bad = || anyhow::anyhow!("svg has a DOCTYPE this reader does not take");
    let space = |i: usize| matches!(x.get(i), Some(b' ' | b'\t' | b'\n' | b'\r'));
    let skip = |mut i: usize| { while space(i) { i += 1; } i };
    let need = |i: usize| if space(i) { Ok(skip(i)) } else { Err(bad()) };
    let name = |i: usize| {
        let n = x[i..].iter().take_while(|b| b.is_ascii_alphanumeric() || b"_-.:".contains(b)).count();
        if n > 0 { Ok(i + n) } else { Err(bad()) }
    };
    // A quoted literal at `i`: its quote, and the index just past its closing quote.
    let quoted = |i: usize| match x.get(i) {
        Some(&q @ (b'"' | b'\'')) => x[i + 1..].iter().position(|&b| b == q).map(|n| (q, i + n + 2)).ok_or_else(bad),
        _ => Err(bad()),
    };
    let mut i = name(need(9)?)?;
    i = skip(i);
    for keyword in [&b"SYSTEM"[..], b"PUBLIC"] {
        if x[i..].starts_with(keyword) {
            i = quoted(need(i + 6)?)?.1;
            if keyword == b"PUBLIC" {
                i = quoted(need(i)?)?.1;
            }
            break;
        }
    }
    i = skip(i);
    match x.get(i) {
        Some(b'>') => return Ok(i + 1),
        Some(b'[') => i += 1,
        _ => return Err(bad()),
    }
    loop {
        i = skip(i);
        if x[i..].starts_with(b"<!ENTITY") {
            i = name(need(i + 8)?)?;
            let (_, end) = quoted(need(i)?)?;
            anyhow::ensure!(!x[i..end].contains(&b'<'), "svg has a DOCTYPE entity that holds markup");
            i = skip(end);
            anyhow::ensure!(x.get(i) == Some(&b'>'), bad());
            i += 1;
        } else if x[i..].starts_with(b"<!--") {
            i += 4 + find(&x[i + 4..], b"-->");
        } else if x.get(i) == Some(&b']') {
            i = skip(i + 1);
            anyhow::ensure!(x.get(i) == Some(&b'>'), bad());
            return Ok(i + 1);
        } else {
            return Err(bad());
        }
    }
}

/// An `.svgz`'s SVG, unpacked on the window's thread to at most `cap` bytes: the product passes `PREVIEW_BINARY_CAP`,
/// the largest SVG the daemon sends at all, so an `.svgz` asks no more of that thread than the same `.svg` would.
/// One layer: a second is refused, since usvg would unpack it itself, past `one_paragraph_svg`, and no SVG format
/// nests gzip. Bytes that are not gzip go on as they are.
fn unpack_svgz(bytes: &[u8], cap: usize) -> Result<Cow<'_, [u8]>> {
    if !bytes.starts_with(&GZIP_MAGIC) {
        return Ok(Cow::Borrowed(bytes));
    }
    let mut svg = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .take(cap as u64 + 1)
        .read_to_end(&mut svg)
        .context("svgz unpack failed")?;
    anyhow::ensure!(svg.len() <= cap, "svgz unpacks past its cap of {cap} bytes");
    anyhow::ensure!(!svg.starts_with(&GZIP_MAGIC), "svgz holds a second gzip layer");
    Ok(Cow::Owned(svg))
}

/// The first two bytes of a gzip stream, an `.svgz`.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

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
#[path = "svg_tests.rs"]
mod tests;
