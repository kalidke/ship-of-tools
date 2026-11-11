//! `sot --draw-svg <heap bytes> <cpu seconds> --version`: the window's drawing child. The window draws only SVG it
//! composes itself; one whose bytes it received is drawn here, in a process the window can lose, so a crash, a hang and
//! an allocation failure are each one refusal. The last argument is never read: a binary without this mode prints its
//! version and exits 0, which the window reads as "not a drawing program".
//!
//! The child bounds itself before it reads a byte (`bound_self`), reads one request from stdin (`svg_wire`: the magic,
//! a u32 job count, then per job an ask and a u64 length with that many bytes of SVG, at most `PREVIEW_BINARY_CAP` in
//! all), draws every job in order on a thread of `SVG_STACK`, and writes one reply per job to stdout, each flushed
//! as it is whole, so a later job that crashes the child costs only itself. The child's own code writes nothing else
//! there; usvg prints a fixed warning line for some color-font text (usvg-0.44.0 text/colr.rs:185, :285), a known
//! writer until a platform primitive gives the reply a private descriptor. Stderr is the window's to close. It ends
//! with exit 0, or by a bound's code (`svg_wire`).

use std::ffi::OsString;
use std::io::{self, Read, Write};

use sot_platform::host::bound::{end_with_parent, exit_now, exit_on_crash};
use sot_platform::host::heap_cap::HeapCap;
use sot_protocol::ir::PREVIEW_BINARY_CAP;

use super::svg::{draw, svg_tree};
use super::svg_wire::*;

/// The stack of the thread that parses and draws, the same on every platform: Windows gives its main thread 1 MiB, and
/// a release build needs 1.2 MiB for 1000 levels of nesting (usvg's limit) and 2.4 for any mask, pattern or filter.
const SVG_STACK: usize = 32 << 20;

/// One job: how to draw, and what.
struct Job {
    size: (u32, u32),
    svg: Vec<u8>,
}

/// Runs the child to its end for `args` (the process's, program name first), whose heap is `heap`, the binary's
/// `#[global_allocator]`: never returns.
pub(crate) fn child_main(heap: &HeapCap, args: &[OsString]) -> ! {
    let number = |at: usize| args.get(at)?.to_str()?.parse::<u64>().ok();
    let ended = match (number(2), number(3)) {
        (Some(heap_bytes), Some(cpu_seconds)) => bound_self(heap, heap_bytes as usize, cpu_seconds)
            .and_then(|()| read_request(io::stdin().lock()))
            .and_then(draw_all),
        _ => Err(invalid("--draw-svg takes a heap size and a CPU bound")),
    };
    exit_now(if ended.is_ok() { 0 } else { EXIT_REFUSED })
}

/// The child's bounds, in the order that matters: no inherited descriptor first (the first act, before any thread or
/// library holds one it keeps), then crashes as exits, the death of the parent, the CPU bound, and the heap armed
/// last of the lot, before the first byte of input.
fn bound_self(heap: &HeapCap, heap_bytes: usize, cpu_seconds: u64) -> io::Result<()> {
    #[cfg(unix)]
    sot_platform::host::bound::close_inherited();
    exit_on_crash(EXIT_CRASH)?;
    end_with_parent()?;
    #[cfg(unix)]
    sot_platform::host::bound::limit_cpu(cpu_seconds, EXIT_CPU)?;
    #[cfg(windows)]
    let _ = cpu_seconds;
    heap.arm(heap_bytes, EXIT_HEAP);
    Ok(())
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn word<const N: usize>(from: &mut impl Read) -> io::Result<[u8; N]> {
    let mut word = [0; N];
    from.read_exact(&mut word)?;
    Ok(word)
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
        if word::<1>(&mut from)? != [ASK_STRETCH] {
            return Err(invalid("an ask this child does not know"));
        }
        let size = (u32::from_le_bytes(word(&mut from)?), u32::from_le_bytes(word(&mut from)?));
        let len = u64::from_le_bytes(word(&mut from)?);
        room = room.checked_sub(len).ok_or_else(|| invalid("input past the cap"))?;
        let mut svg = Vec::with_capacity(len as usize);
        if from.by_ref().take(len).read_to_end(&mut svg)? as u64 != len {
            return Err(invalid("input ends early"));
        }
        jobs.push(Job { size, svg });
    }
    Ok(jobs)
}

/// Every job drawn in order on a thread of `SVG_STACK`, one reply each, flushed as it is whole (the child ends by
/// `_exit`, which flushes nothing). A panic on that thread ends the child with `EXIT_CRASH` at once, on every system.
fn draw_all(jobs: Vec<Job>) -> io::Result<()> {
    let thread = std::thread::Builder::new().name("svg".into()).stack_size(SVG_STACK).spawn(move || {
        let mut out = io::stdout().lock();
        out.write_all(&MAGIC)?;
        out.flush()?;
        for job in &jobs {
            reply(&mut out, job)?;
            out.flush()?;
        }
        Ok(())
    });
    thread?.join().unwrap_or_else(|_| exit_now(EXIT_CRASH))
}

fn reply(out: &mut impl Write, job: &Job) -> io::Result<()> {
    match svg_tree(&job.svg).and_then(|tree| draw(&tree, job.size.0, job.size.1)) {
        Ok(pixmap) => {
            out.write_all(&[REPLY_PIXELS])?;
            out.write_all(&pixmap.width().to_le_bytes())?;
            out.write_all(&pixmap.height().to_le_bytes())?;
            out.write_all(pixmap.data())
        }
        Err(error) => {
            let text = format!("{error:#}");
            let text = first_line(&text, ERROR_BYTES);
            out.write_all(&[REPLY_ERROR])?;
            out.write_all(&(text.len() as u16).to_le_bytes())?;
            out.write_all(text.as_bytes())
        }
    }
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
