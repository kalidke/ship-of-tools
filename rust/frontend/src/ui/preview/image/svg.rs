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
use resvg::usvg::roxmltree::{self, ParsingOptions};
use resvg::usvg::{fontdb, ImageHrefResolver, ImageKind, Options, Tree};

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
    let pixmap = on_svg_stack(|| draw(&svg_tree(bytes)?, target_w, target_h))?;
    Quad::from_rgba8(device, queue, pipeline, pixmap.data(), pixmap.width(), pixmap.height())
}

/// `tree` drawn into a `w` by `h` pixmap (a side under one pixel is one), stretched to fill it.
fn draw(tree: &Tree, w: u32, h: u32) -> Result<Pixmap> {
    let (w, h) = (w.max(1), h.max(1));
    let size = tree.size();
    let mut pixmap = Pixmap::new(w, h).ok_or_else(|| anyhow::anyhow!("pixmap alloc failed for {w}x{h}"))?;
    let (sx, sy) = (w as f32 / size.width().max(0.001), h as f32 / size.height().max(0.001));
    resvg::render(tree, Transform::from_scale(sx, sy), &mut pixmap.as_mut());
    Ok(pixmap)
}

/// The SVG's tree as the window draws it: system fonts, `currentColor` as white, its text with no bidi paragraph
/// separator (`one_paragraph_svg`), an XML shape the parser cannot be crashed by (`check_xml_shape`) and no file read
/// for an `<image>` (`embedded_images_only`). An `.svgz` (gzip) is unpacked first (`unpack_svgz`): usvg would unpack it
/// itself, past all three.
fn svg_tree(bytes: &[u8]) -> Result<Tree> {
    let opts = Options {
        fontdb: system_fontdb(),
        image_href_resolver: embedded_images_only(),
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
    let text = std::str::from_utf8(&bytes).context("svg is not UTF-8")?;
    let xml = ParsingOptions { allow_dtd: true, ..ParsingOptions::default() };
    let doc = roxmltree::Document::parse_with_options(text, xml).context("svg parse failed")?;
    check_reference_cost(&doc)?;
    #[allow(clippy::disallowed_methods, reason = "text into the shaper: an SVG's one way into usvg, through one_paragraph_svg")]
    let tree = Tree::from_xmltree(&doc, &opts);
    tree.context("svg parse failed")
}

/// The stack of the thread that parses and draws an SVG, the same on every platform: Windows gives its main thread
/// 1 MiB, and a release build needs 1.2 MiB for 1000 levels of nesting and 2.4 for any mask, pattern or filter.
/// Measured in a release build, in KiB of stack per level of nesting or link of a chain of references: nesting 1.2,
/// `clip-path` 2.6, `mask` 2.8, `use` 4.7, `pattern` 5.6, `filter` and `marker` 5.9. `MAX_RECURSION_UNITS` holds a
/// document to about 5 MiB above that base, so the 32 leave a release build six times the room, and a debug build,
/// which needs several times the stack, draws the bounds too.
const SVG_STACK: usize = 32 << 20;

/// `f` on a thread of `SVG_STACK` bytes, joined before this returns: what recurses once per element or per link of a
/// chain of references runs here, so its limit does not depend on the caller's thread, and a panic is an error.
fn on_svg_stack<T: Send>(f: impl FnOnce() -> Result<T> + Send) -> Result<T> {
    std::thread::scope(|scope| {
        let thread = std::thread::Builder::new().name("svg".into()).stack_size(SVG_STACK).spawn_scoped(scope, f);
        thread.context("svg thread did not start")?.join().map_err(|_| anyhow::anyhow!("svg drawing panicked"))?
    })
}

/// A unit of recursion is one level of XML nesting, 1.25 KiB of stack. A link of a chain of references costs the depth
/// of the element that holds it below its owner (the nearest element with an id) plus `REF_COST`, over the 5.9 KiB
/// the dearest kind measured.
const REF_COST: u64 = 4;

/// The most recursion units a document may need: usvg's 1024 levels of nesting, or a chain of 818 `clip-path` links.
const MAX_RECURSION_UNITS: u64 = 4096;

/// Refuses a document whose recursion would pass `MAX_RECURSION_UNITS`, before usvg converts it: the cost of the
/// costliest path through its nesting and its references (`url(#id)` and `#id` anywhere, and in a `<style>`, which may
/// reach any element). A chain of references nests usvg's converter and resvg's renderer with no XML nesting, so 20000
/// `clip-path` links overflow any stack. The nodes are the document and the elements with an `id`; an edge is the
/// nesting down to the next such element, or a reference, at its cost. Kahn's algorithm finds the longest path with no
/// recursion; what a cycle holds (usvg breaks them) is bounded by its node count times the dearest edge.
fn check_reference_cost(doc: &roxmltree::Document) -> Result<()> {
    use std::collections::HashMap;
    let n = doc.descendants().count() + 1;
    let hub = n - 1;
    let slot = |node: roxmltree::Node| node.id().get_usize();
    let elements: Vec<_> = doc.descendants().filter(|e| e.is_element()).collect();
    let (mut depth, mut owner, mut deepest) = (vec![0u64; n], vec![0usize; n], vec![0u64; n]);
    let mut gradient = vec![false; n];
    let (mut ids, mut edges): (HashMap<&str, Vec<usize>>, Vec<(usize, usize, u64)>) = (HashMap::new(), Vec::new());
    for e in &elements {
        let (i, parent) = (slot(*e), slot(e.parent().expect("an element has a parent")));
        depth[i] = depth[parent] + 1;
        deepest[i] = depth[i];
        gradient[i] = matches!(e.tag_name().name(), "linearGradient" | "radialGradient");
        owner[i] = owner[parent];
        if let Some(id) = e.attribute("id") {
            ids.entry(id).or_default().push(i);
            edges.push((owner[parent], i, depth[i] - depth[owner[parent]]));
            owner[i] = i;
        }
    }
    for e in elements.iter().rev() {
        let parent = slot(e.parent().expect("an element has a parent"));
        deepest[parent] = deepest[parent].max(deepest[slot(*e)]);
    }
    let max_depth = deepest[0];
    let mut styled = false;
    for e in &elements {
        let (i, is_style) = (slot(*e), e.has_tag_name("style"));
        let from = if is_style { hub } else { owner[i] };
        let cost = (if is_style { max_depth } else { depth[i] - depth[from] }) + REF_COST;
        let mut refer = |text: &str| {
            for target in referenced_ids(text).filter_map(|id| ids.get(id)).flatten() {
                // A gradient holds only stops, so a stylesheet's `fill` of one continues no chain.
                if !(is_style && gradient[*target]) {
                    edges.push((from, *target, cost));
                }
            }
        };
        e.attributes().for_each(|a| refer(a.value()));
        if is_style {
            styled = true;
            e.descendants().filter_map(|d| d.text()).for_each(refer);
        }
    }
    if styled {
        let ided: Vec<usize> = ids.values().flatten().copied().collect();
        edges.extend(std::iter::once(0).chain(ided).map(|from| (from, hub, 0)));
    }
    let dearest = edges.iter().map(|e| e.2).max().unwrap_or(0);
    let (mut indegree, mut first) = (vec![0u32; n], vec![0usize; n + 1]);
    edges.sort_unstable_by_key(|e| e.0);
    for &(from, to, _) in &edges {
        indegree[to] += 1;
        first[from + 1] += 1;
    }
    for v in 0..n {
        first[v + 1] += first[v];
    }
    let mut dist = vec![0u64; n];
    let mut ready: Vec<usize> = (0..n).filter(|&v| indegree[v] == 0).collect();
    let (mut seen, mut worst, mut reach) = (0, 0u64, 0u64);
    while let Some(v) = ready.pop() {
        seen += 1;
        worst = worst.max(dist[v] + deepest[v] - depth[v]);
        reach = reach.max(dist[v]);
        for &(_, to, cost) in &edges[first[v]..first[v + 1]] {
            dist[to] = dist[to].max(dist[v] + cost);
            indegree[to] -= 1;
            if indegree[to] == 0 {
                ready.push(to);
            }
        }
    }
    let cyclic = (n - seen) as u64;
    let cost = if cyclic > 0 { worst.max(reach + cyclic * dearest + max_depth) } else { worst };
    anyhow::ensure!(cost <= MAX_RECURSION_UNITS, "svg nests {cost} deep through its elements and references, past {MAX_RECURSION_UNITS}");
    Ok(())
}

/// The ids `value` refers to: `#id` as a whole (an href), and each `url(#id)` in it.
fn referenced_ids(value: &str) -> impl Iterator<Item = &str> {
    let urls = value.match_indices("url(").filter_map(|(at, _)| {
        let rest = value[at + 4..].trim_start().trim_start_matches(['"', '\'']).strip_prefix('#')?;
        Some(&rest[..rest.find([')', '"', '\'', ' ']).unwrap_or(rest.len())])
    });
    value.strip_prefix('#').into_iter().chain(urls)
}

/// usvg's own bound on element nesting, which it checks only after roxmltree has parsed the document, and roxmltree
/// recurses once per element: a 20000-deep file aborts the process first. A scan before the parse loses nothing drawable.
const SVG_MAX_DEPTH: usize = 1024;

/// The decoded pixels the embedded images of one document may add up to: 64 Mpx, a 4K screen's eight. usvg asks the
/// resolver once for each `<image>` it converts and for each `<use>` of one, and resvg decodes again for each, so
/// without a sum an 8-pixel-wide file repeating one embedded raster holds the window's thread for minutes.
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

/// The only `<image>` sources the window draws: embedded (`data:`) PNG, JPEG, GIF and WebP, in a document's
/// `MAX_IMAGE_PIXELS`, each by the size its header declares. A path or URL names a file on the window's computer
/// (usvg's default reads any path that exists: a FIFO hangs the thread, `/dev/zero` fills memory) and is refused; so is
/// nested SVG data, which usvg parses past `check_xml_shape`, and data with no image type or `text/plain`, which
/// usvg's default sniffs.
fn embedded_images_only() -> ImageHrefResolver<'static> {
    let spent = std::sync::atomic::AtomicU64::new(0);
    ImageHrefResolver {
        resolve_data: Box::new(move |mime, data, _| {
            let kind: fn(Arc<Vec<u8>>) -> ImageKind = match mime {
                "image/jpg" | "image/jpeg" => ImageKind::JPEG,
                "image/png" => ImageKind::PNG,
                "image/gif" => ImageKind::GIF,
                "image/webp" => ImageKind::WEBP,
                _ => return None,
            };
            let (w, h) = {
                let decoder = image::ImageReader::new(std::io::Cursor::new(&data[..])).with_guessed_format().ok()?.into_decoder().ok()?;
                image::ImageDecoder::dimensions(&decoder)
            };
            let pixels = u64::from(w) * u64::from(h);
            let total = spent.fetch_add(pixels, std::sync::atomic::Ordering::Relaxed) + pixels;
            (total <= MAX_IMAGE_PIXELS).then(|| kind(data))
        }),
        resolve_string: Box::new(|_, _| None),
    }
}

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
