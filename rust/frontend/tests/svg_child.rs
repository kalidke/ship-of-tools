//! The drawing child, `sot --draw-svg`: the built binary started the way the window starts it (a cleared environment
//! with `HOME`, stderr null), a request on stdin, and its replies and its end read back. Each bound has the file that
//! meets it. A child that crashes or spins runs under a core limit and in a scratch folder, so nothing it ends by can
//! leave a core or a report outside that folder.

#[path = "../src/ui/preview/image/svg_wire.rs"]
mod wire;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use wire::*;

const MIB: usize = 1 << 20;
const WAIT: Duration = Duration::from_secs(60);
const SVG_OPEN: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">"#;

/// A scratch folder: the child's working directory and `HOME`, removed with this value.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("sot-svg-child-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&path).expect("make the scratch folder");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How a child ended, what it wrote, and how long it lived.
struct Ran {
    status: ExitStatus,
    out: Vec<u8>,
    lived: Duration,
}

/// The child with `heap` bytes of heap and `cpu` seconds of CPU, given `input` on stdin, waited for `WAIT`. On Unix it
/// runs under a core limit of one byte, which also stops a piped crash handler (a limit of zero does not on every
/// system), and with the scratch folder as its working directory.
fn run(heap: usize, cpu: u64, input: Vec<u8>) -> Ran {
    run_within(WAIT, heap, cpu, input)
}

/// `run`, with the child killed (and the test failed) if it has not ended within `wait`.
fn run_within(wait: Duration, heap: usize, cpu: u64, input: Vec<u8>) -> Ran {
    let scratch = Scratch::new();
    let args = ["--draw-svg", &heap.to_string(), &cpu.to_string(), "--version"].map(String::from);
    let bin = env!("CARGO_BIN_EXE_sot");
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "ulimit -c 1; exec \"$0\" \"$@\"", bin]);
        command
    };
    #[cfg(not(unix))]
    let mut command = Command::new(bin);
    command
        .args(&args)
        .env_clear()
        .env("HOME", &scratch.0)
        .current_dir(&scratch.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // What the font code reads on Windows (the plan's P13); nothing else of the test's environment goes through.
    #[cfg(windows)]
    for name in ["SYSTEMROOT", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let started = Instant::now();
    let mut child = command.spawn().expect("start the child");
    let (mut stdin, mut stdout) = (child.stdin.take().unwrap(), child.stdout.take().unwrap());
    // A child that ends before it has read its input closes the pipe under the writer: that is its answer, not an error.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        out
    });
    let status = sot_platform::test_isolated::wait_within(&mut child, wait);
    let lived = started.elapsed();
    writer.join().unwrap();
    Ran { status, out: reader.join().unwrap(), lived }
}

/// A request for `jobs`, each `(w, h, svg)`.
fn request(jobs: &[(u32, u32, &[u8])]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend((jobs.len() as u32).to_le_bytes());
    for (w, h, svg) in jobs {
        out.push(ASK_STRETCH);
        out.extend(w.to_le_bytes());
        out.extend(h.to_le_bytes());
        out.extend((svg.len() as u64).to_le_bytes());
        out.extend(*svg);
    }
    out
}

#[derive(Debug, PartialEq)]
enum Reply {
    Pixels { w: u32, h: u32, rgba: Vec<u8> },
    Error(String),
}

/// The replies in `out`, which must be whole: the magic, then one reply per job and not a byte more.
fn replies(out: &[u8]) -> Vec<Reply> {
    fn take<'a>(rest: &mut &'a [u8], n: usize) -> &'a [u8] {
        assert!(rest.len() >= n, "a reply is cut short");
        let (head, tail) = rest.split_at(n);
        *rest = tail;
        head
    }
    let word = |rest: &mut &[u8]| u32::from_le_bytes(take(rest, 4).try_into().unwrap());
    let mut rest = out;
    assert_eq!(take(&mut rest, 4), MAGIC, "the reply opens with the magic");
    let mut replies = Vec::new();
    while !rest.is_empty() {
        match take(&mut rest, 1)[0] {
            REPLY_PIXELS => {
                let (w, h) = (word(&mut rest), word(&mut rest));
                replies.push(Reply::Pixels { w, h, rgba: take(&mut rest, 4 * w as usize * h as usize).to_vec() });
            }
            REPLY_ERROR => {
                let len = u16::from_le_bytes(take(&mut rest, 2).try_into().unwrap()) as usize;
                assert!(len <= ERROR_BYTES, "an error text is at most {ERROR_BYTES} bytes");
                replies.push(Reply::Error(String::from_utf8(take(&mut rest, len).to_vec()).expect("an error text is UTF-8")));
            }
            tag => panic!("a reply tagged {tag}"),
        }
    }
    replies
}

fn pixel(reply: &Reply, x: u32, y: u32) -> [u8; 4] {
    let Reply::Pixels { w, rgba, .. } = reply else { panic!("{reply:?} is no picture") };
    let at = 4 * (y * w + x) as usize;
    rgba[at..at + 4].try_into().unwrap()
}

fn rect(fill: &str) -> String {
    format!(r#"{SVG_OPEN}<rect width="10" height="10" fill="{fill}"/></svg>"#)
}

/// The heap every test but the heap tests gives the child.
const ROOMY: usize = 1 << 30;

#[test]
fn a_plain_svg_comes_back_as_its_pixels() {
    let ran = run(ROOMY, 30, request(&[(20, 20, rect("red").as_bytes())]));
    assert_eq!(ran.status.code(), Some(0), "a drawn request ends with 0");
    let replies = replies(&ran.out);
    assert_eq!(replies.len(), 1);
    assert!(matches!(&replies[0], Reply::Pixels { w: 20, h: 20, .. }));
    assert_eq!(pixel(&replies[0], 10, 10), [255, 0, 0, 255]);
}

#[test]
fn every_job_of_a_request_is_drawn_in_order_and_an_error_does_not_end_the_child() {
    let (red, blue) = (rect("red"), rect("blue"));
    let ran = run(
        ROOMY,
        30,
        request(&[(4, 4, red.as_bytes()), (6, 2, b"this is no SVG"), (8, 8, blue.as_bytes()), (4, 4, red.as_bytes())]),
    );
    assert_eq!(ran.status.code(), Some(0));
    let replies = replies(&ran.out);
    assert_eq!(replies.len(), 4, "one reply per job");
    assert_eq!(pixel(&replies[0], 2, 2), [255, 0, 0, 255]);
    assert!(matches!(&replies[1], Reply::Error(text) if text.starts_with("svg parse failed")), "{:?}", replies[1]);
    assert!(matches!(&replies[2], Reply::Pixels { w: 8, h: 8, .. }));
    assert_eq!(pixel(&replies[2], 4, 4), [0, 0, 255, 255]);
    assert_eq!(pixel(&replies[3], 2, 2), [255, 0, 0, 255]);
}

#[test]
fn a_request_that_is_no_request_is_refused_with_its_code() {
    for input in [b"no magic, just text".to_vec(), MAGIC.to_vec(), request(&[(2, 2, b"<svg/>")])[..30].to_vec()] {
        let ran = run(ROOMY, 30, input);
        assert_eq!(ran.status.code(), Some(EXIT_REFUSED));
        assert!(ran.out.is_empty(), "nothing but replies reaches stdout");
    }
    // Past the cap by one job alone, and by the sum of two: a heap of 64 MiB would end a child that allocated either
    // length, so the refusal has to come first.
    let cap = sot_protocol::ir::PREVIEW_BINARY_CAP as u64;
    let declared = |jobs: &[&[u8]], last: u64| {
        let mut input = request(&jobs.iter().map(|svg| (2, 2, *svg)).collect::<Vec<_>>());
        input.extend([ASK_STRETCH, 2, 0, 0, 0, 2, 0, 0, 0]);
        input.extend(last.to_le_bytes());
        input[4..8].copy_from_slice(&(jobs.len() as u32 + 1).to_le_bytes());
        input
    };
    let first = rect("red").into_bytes();
    for input in [declared(&[], cap + 1), declared(&[&first], cap)] {
        let ran = run(64 * MIB, 30, input);
        assert_eq!(ran.status.code(), Some(EXIT_REFUSED), "input past the cap is a refusal, before any allocation");
    }
}

#[test]
fn an_input_past_the_heap_ends_with_its_code() {
    let mut svg = format!("{SVG_OPEN}<!--").into_bytes();
    svg.resize(100 * MIB, b'a');
    svg.extend(b"--></svg>");
    let ran = run(64 * MIB, 30, request(&[(2, 2, &svg)]));
    assert_eq!(ran.status.code(), Some(EXIT_HEAP));
}

#[test]
fn a_draw_past_its_heap_ends_with_its_code() {
    let ran = run(64 * MIB, 30, request(&[(8192, 8192, rect("red").as_bytes())]));
    assert_eq!(ran.status.code(), Some(EXIT_HEAP));
}

#[test]
fn an_entity_expansion_past_its_heap_ends_with_its_code() {
    let mut svg = format!("<!DOCTYPE svg [<!ENTITY a \"{}\">]>{SVG_OPEN}<rect width=\"10\" height=\"10\" id=\"", "x".repeat(MIB));
    svg.push_str(&"&a;".repeat(200));
    svg.push_str("\"/></svg>");
    let ran = run(64 * MIB, 30, request(&[(2, 2, svg.as_bytes())]));
    assert_eq!(ran.status.code(), Some(EXIT_HEAP));
}

/// A pattern whose rectangle is masked, by a mask that fills with the pattern: usvg breaks a cycle within one kind of
/// reference and recurses without end across two.
const CYCLE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><defs>
<pattern id="p" width="10" height="10" patternUnits="userSpaceOnUse"><rect width="10" height="10" mask="url(#k)"/></pattern>
<mask id="k"><rect width="100" height="100" fill="url(#p)"/></mask></defs>
<rect width="100" height="100" fill="url(#p)"/></svg>"##;

#[test]
fn a_reference_cycle_across_two_kinds_ends_the_child_and_not_the_caller() {
    let ran = run(ROOMY, 30, request(&[(100, 100, CYCLE.as_bytes())]));
    if cfg!(windows) {
        // The OS's own crash status stands in for the code (P8): no dialog can be seen on a hosted runner, so what
        // this pins is an end, not a success, and a prompt one; the time is printed for the record.
        eprintln!("the cycle's child ended in {:?} with {}", ran.lived, ran.status);
        assert!(!ran.status.success());
        assert!(ran.lived < Duration::from_secs(20), "the end is prompt, not held by error reporting");
    } else {
        assert_eq!(ran.status.code(), Some(EXIT_CRASH), "an abort is an exit with the crash code, not SIGABRT");
    }
    assert!(ran.out.is_empty() || ran.out == MAGIC, "no picture came back from a crashed draw");
}

/// 100 million octaves of noise: a few hundred bytes that spin for weeks.
#[cfg(unix)]
const SPIN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><filter id="f">
<feTurbulence baseFrequency="0.05" numOctaves="100000000"/></filter><rect width="100" height="100" filter="url(#f)"/></svg>"##;

#[cfg(unix)]
#[test]
fn a_child_past_its_cpu_bound_ends_itself() {
    let ran = run_within(Duration::from_secs(10), ROOMY, 1, request(&[(100, 100, SPIN.as_bytes())]));
    assert_eq!(ran.status.code(), Some(EXIT_CPU), "the CPU bound is an exit with its code, not SIGXCPU");
    assert!(ran.lived < Duration::from_secs(10), "{:?}", ran.lived);
}
