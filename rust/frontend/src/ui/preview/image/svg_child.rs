//! `sot --draw-svg <heap bytes> <cpu seconds> --version`: the window's drawing child. The window draws only SVG it
//! composes itself; one whose bytes it received is drawn here, in a process the window can lose, so a crash, a hang and
//! an allocation failure are each one refusal. The last argument is never read: a binary without this mode prints its
//! version and exits 0, which the window reads as "not a drawing program".
//!
//! The child bounds itself before it reads a byte (`bound_self`), reads one request from stdin (`svg_wire`: the magic,
//! a u32 job count, then per job an ask (`Ask`: a stretch to a size, a fit to a pane, whose size the child works out, or a chat picture's original to decode and answer as PNG fitted to `MAX_RASTER_PIXELS`) and a u64 length with that many bytes of SVG or picture, at most `PREVIEW_BINARY_CAP` in
//! all), draws every job in order on a thread of `SVG_STACK`, and writes one reply per job to the process's stdout,
//! taken for the reply alone before the read (`take_stdout`: descriptor 1 then leads to the null device), each written
//! as it is whole, so a later job that crashes the child costs only itself. usvg prints a fixed warning line for some
//! color-font text (usvg-0.44.0 text/colr.rs:185, :285); that, and any other library's print, goes to the null device.
//! Stderr is the window's to close. It ends with exit 0, or by a bound's code (`svg_wire`).

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};

use sot_platform::host::bound::{end_with_parent, exit_now, exit_on_crash, take_stdout};
use sot_platform::host::heap_cap::HeapCap;
use sot_protocol::ir::PREVIEW_BINARY_CAP;

use super::svg::{draw, svg_fit_dims, svg_tree};
use super::svg_wire::*;

/// The stack of the thread that parses and draws, the same on every platform: Windows gives its main thread 1 MiB, and
/// a release build needs 1.2 MiB for 1000 levels of nesting (usvg's limit) and 2.4 for any mask, pattern or filter.
const SVG_STACK: usize = 32 << 20;

/// One job: how to draw, and what.
struct Job {
    ask: Ask,
    svg: Vec<u8>,
}

/// Runs the child to its end for `args` (the process's, program name first), whose heap is `heap`, the binary's
/// `#[global_allocator]`: never returns.
pub(crate) fn child_main(heap: &HeapCap, args: &[OsString]) -> ! {
    let number = |at: usize| args.get(at)?.to_str()?.parse::<u64>().ok();
    let ended = match (number(2), number(3)) {
        (Some(heap_bytes), Some(cpu_seconds)) => bound_self(heap, heap_bytes as usize, cpu_seconds).and_then(|out| {
            let jobs = read_request(io::stdin().lock())?;
            draw_all(jobs, out)
        }),
        _ => Err(invalid("--draw-svg takes a heap size and a CPU bound")),
    };
    exit_now(if ended.is_ok() { 0 } else { EXIT_REFUSED })
}

/// The child's bounds, in the order that matters: no inherited descriptor first (the first act, before any thread or
/// library holds one it keeps), then stdout taken for the reply (after the close, which would take its copy), crashes
/// as exits, the death of the parent, the CPU bound, and the heap armed last of the lot, before the first byte of
/// input. Returns the file the replies are written to.
fn bound_self(heap: &HeapCap, heap_bytes: usize, cpu_seconds: u64) -> io::Result<File> {
    #[cfg(unix)]
    sot_platform::host::bound::close_inherited();
    let out = take_stdout()?;
    exit_on_crash(EXIT_CRASH)?;
    end_with_parent()?;
    #[cfg(unix)]
    sot_platform::host::bound::limit_cpu(cpu_seconds, EXIT_CPU)?;
    #[cfg(windows)]
    let _ = cpu_seconds;
    heap.arm(heap_bytes, EXIT_HEAP);
    Ok(out)
}

/// The request on `from`. A job's bytes are allocated at their declared length, which the heap cap sees before they
/// are read; their sum may not pass `PREVIEW_BINARY_CAP`.
fn read_request(mut from: impl Read) -> io::Result<Vec<Job>> {
    if word::<4>(&mut from)? != MAGIC {
        return Err(invalid("not a drawing request"));
    }
    let (count, mut room) = (u32::from_le_bytes(word(&mut from)?), PREVIEW_BINARY_CAP as u64);
    let mut jobs = Vec::new();
    for _ in 0..count {
        let ask = read_ask(&mut from)?;
        let len = u64::from_le_bytes(word(&mut from)?);
        room = room.checked_sub(len).ok_or_else(|| invalid("input past the cap"))?;
        let mut svg = Vec::with_capacity(len as usize);
        if from.by_ref().take(len).read_to_end(&mut svg)? as u64 != len {
            return Err(invalid("input ends early"));
        }
        jobs.push(Job { ask, svg });
    }
    Ok(jobs)
}

/// The next ask on `from`.
fn read_ask(from: &mut impl Read) -> io::Result<Ask> {
    let tag = word::<1>(from)?[0];
    let mut u32 = || Ok::<_, io::Error>(u32::from_le_bytes(word(from)?));
    match tag {
        ASK_STRETCH => Ok(Ask::Stretch(u32()?, u32()?)),
        ASK_FIT => Ok(Ask::Fit { pane: (u32()?, u32()?), cap: u32()? }),
        ASK_PICTURE => Ok(Ask::Picture { cap: u32()? }),
        _ => Err(invalid("an ask this child does not know")),
    }
}

/// Every job drawn in order on a thread of `SVG_STACK`, one reply each written to `out` (a file, so unbuffered: the
/// child ends by `_exit`, which flushes nothing). A panic on that thread ends the child with `EXIT_CRASH` at once, on
/// every system.
fn draw_all(jobs: Vec<Job>, mut out: File) -> io::Result<()> {
    let thread = std::thread::Builder::new().name("svg".into()).stack_size(SVG_STACK).spawn(move || {
        out.write_all(&MAGIC)?;
        for job in &jobs {
            #[cfg(debug_assertions)]
            stray_print();
            reply(&mut out, job)?;
        }
        Ok(())
    });
    thread?.join().unwrap_or_else(|_| exit_now(EXIT_CRASH))
}

/// Test only (a debug build): `SOT_SVG_CHILD_STRAY` makes the child print to its stdout between replies, as usvg does
/// for some color-font text, so the test can see the line never reach the reply stream.
#[cfg(debug_assertions)]
fn stray_print() {
    if std::env::var_os("SOT_SVG_CHILD_STRAY").is_some() {
        println!("Warning: a stray line between two replies");
    }
}

fn reply(out: &mut impl Write, job: &Job) -> io::Result<()> {
    let drawn = match job.ask {
        Ask::Picture { cap } => {
            return match picture(&job.svg, cap) {
                Ok((w, h, png)) => {
                    out.write_all(&[REPLY_PNG])?;
                    out.write_all(&w.to_le_bytes())?;
                    out.write_all(&h.to_le_bytes())?;
                    out.write_all(&(png.len() as u64).to_le_bytes())?;
                    out.write_all(&png)
                }
                Err(error) => error_reply(out, &error),
            };
        }
        Ask::Stretch(w, h) => svg_tree(&job.svg).and_then(|tree| draw(&tree, w, h)),
        Ask::Fit { pane, cap } => svg_tree(&job.svg).and_then(|tree| {
            let (w, h) = svg_fit_dims((tree.size().width(), tree.size().height()), pane, cap);
            draw(&tree, w, h)
        }),
    };
    match drawn {
        Ok(pixmap) => {
            out.write_all(&[REPLY_PIXELS])?;
            out.write_all(&pixmap.width().to_le_bytes())?;
            out.write_all(&pixmap.height().to_le_bytes())?;
            out.write_all(pixmap.data())
        }
        Err(error) => error_reply(out, &error),
    }
}

fn error_reply(out: &mut impl Write, error: &anyhow::Error) -> io::Result<()> {
    let text = format!("{error:#}");
    let text = first_line(&text, ERROR_BYTES);
    out.write_all(&[REPLY_ERROR])?;
    out.write_all(&(text.len() as u16).to_le_bytes())?;
    out.write_all(text.as_bytes())
}

/// A chat picture's original (`bytes`, one of wire's `PICTURE_MIMES` by its own bytes) decoded under the image crate's
/// default `Limits` and this child's own bounds, fitted to its aspect with no side past `cap` and at most
/// `MAX_RASTER_PIXELS`, and encoded as PNG: its size and bytes.
fn picture(bytes: &[u8], cap: u32) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    use anyhow::Context;
    use image::ImageEncoder;
    let mut reader = image::ImageReader::new(io::Cursor::new(bytes)).with_guessed_format().context("not a picture")?;
    let format = reader.format().filter(|f| sot_protocol::chat::PICTURE_MIMES.contains(&f.to_mime_type()));
    anyhow::ensure!(format.is_some(), "not a picture the chat shows");
    reader.limits(image::Limits::default());
    let rgba = image::DynamicImage::from_decoder(reader.into_decoder().context("not a picture")?).context("it does not decode")?.to_rgba8();
    let (w, h) = picture_fit(rgba.dimensions(), cap);
    let rgba = if (w, h) == rgba.dimensions() { rgba } else { image::imageops::thumbnail(&rgba, w, h) };
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Sub)
        .write_image(rgba.as_raw(), w, h, image::ExtendedColorType::Rgba8)
        .context("it does not encode")?;
    Ok((w, h, png))
}

/// `(w, h)` scaled down, aspect kept, until no side passes `cap` and the whole is at most `MAX_RASTER_PIXELS`; never up.
fn picture_fit((w, h): (u32, u32), cap: u32) -> (u32, u32) {
    let (w64, h64) = (u64::from(w.max(1)), u64::from(h.max(1)));
    let by_side = f64::from(cap.max(1)) / w64.max(h64) as f64;
    let by_area = (MAX_RASTER_PIXELS as f64 / (w64 * h64) as f64).sqrt();
    let by = by_side.min(by_area).min(1.0);
    let side = |s: u64| ((s as f64 * by).floor() as u32).clamp(1, cap.max(1));
    (side(w64), side(h64))
}

/// `text`'s first line, cut to at most `limit` bytes at a character boundary.
fn first_line(text: &str, limit: usize) -> &str {
    let line = text.lines().next().unwrap_or("");
    let mut end = line.len().min(limit);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
#[path = "svg_child_tests.rs"]
mod tests;
