//! The drawing child, `sot --draw-svg`: the built binary started the way the window starts it (a cleared environment
//! with `HOME`, stderr null), a request on stdin, and its replies and its end read back. Each bound has the file that
//! meets it. A child that crashes or spins runs under a core limit and in a scratch folder, so nothing it ends by can
//! leave a core or a report outside that folder. The runner (`svg_run`, the window's half) is tested here too, on the
//! real child and on stand-ins for it. On Unix the runner's child is started through a shell that sets `HOME` to the
//! scratch folder and applies the core limit; on Windows `start` passes the test's own `SYSTEMROOT` and `USERPROFILE`.

// The two files side by side at the root, as they are in the crate, so that the runner's `super::svg_wire` resolves.
#[path = "../src/ui/preview/image/svg_wire.rs"]
mod svg_wire;
#[path = "../src/ui/preview/image/svg_run.rs"]
#[allow(dead_code, reason = "what the window uses and this target does not: the program's path and the real bounds")]
mod svg_run;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use svg_run::{child_env, draw_in_child, finish, killed_by_us, start, Bounds, Job, Outcome, Stop};
use svg_wire::*;

const MIB: usize = 1 << 20;
const WAIT: Duration = Duration::from_secs(60);
/// Run under a core limit of one byte, which also stops a piped crash handler (zero does not on every system).
#[cfg(unix)]
const WRAP: &str = "ulimit -c 1; exec \"$0\" \"$@\"";
/// What the runner says of a reply that breaks its form.
#[cfg(unix)]
const BAD_REPLY: &str = "the drawing's reply was not what was asked";
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
    run_with_env(wait, heap, cpu, input, &[])
}

/// `run_within`, with `extra` added to the child's cleared environment.
fn run_with_env(wait: Duration, heap: usize, cpu: u64, input: Vec<u8>, extra: &[(&str, &str)]) -> Ran {
    let scratch = Scratch::new();
    let args = ["--draw-svg", &heap.to_string(), &cpu.to_string(), "--version"].map(String::from);
    let bin = env!("CARGO_BIN_EXE_sot");
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", WRAP, bin]);
        command
    };
    #[cfg(not(unix))]
    let mut command = Command::new(bin);
    command
        .args(&args)
        .env_clear()
        .env("HOME", &scratch.0)
        .envs(extra.iter().copied())
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
        out.extend(Ask::Stretch(*w, *h).to_bytes());
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

/// A request for one job drawn to fit `pane` under `cap`.
fn fit_request(pane: (u32, u32), cap: u32, svg: &[u8]) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend(1u32.to_le_bytes());
    out.extend(Ask::Fit { pane, cap }.to_bytes());
    out.extend((svg.len() as u64).to_le_bytes());
    out.extend(svg);
    out
}

#[test]
fn a_fit_ask_is_drawn_at_the_size_the_child_works_out() {
    // A 10 by 10 SVG fitted to a 400 by 200 pane is 200 by 200; a texture limit of 50 holds it to 50 by 50.
    for (cap, side) in [(16384, 200), (50, 50)] {
        let ran = run(ROOMY, 30, fit_request((400, 200), cap, rect("red").as_bytes()));
        assert_eq!(ran.status.code(), Some(0));
        let replies = replies(&ran.out);
        assert!(matches!(&replies[0], Reply::Pixels { w, h, .. } if (*w, *h) == (side, side)), "cap {cap}: {:?}", replies[0]);
        assert_eq!(pixel(&replies[0], side / 2, side / 2), [255, 0, 0, 255]);
    }
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
    assert_eq!(ran.out, MAGIC, "the magic was delivered and no picture came back from a crashed draw");
}

#[test]
fn replies_before_a_crash_arrive_whole() {
    // The child ends by `_exit`, which flushes nothing: each reply has to leave the child as it is finished, and the
    // small ones are the ones a line buffer would keep.
    let red = rect("red");
    let ran = run(ROOMY, 30, request(&[(6, 6, b"this is no SVG"), (1, 1, red.as_bytes()), (100, 100, CYCLE.as_bytes())]));
    if !cfg!(windows) {
        assert_eq!(ran.status.code(), Some(EXIT_CRASH));
    }
    let replies = replies(&ran.out);
    assert_eq!(replies.len(), 2, "the two jobs before the crash were answered");
    assert!(matches!(&replies[0], Reply::Error(text) if text.starts_with("svg parse failed")));
    assert_eq!(pixel(&replies[1], 0, 0), [255, 0, 0, 255]);
}

/// The child prints a line to its own stdout between replies, as usvg does for some color-font text (a debug build's
/// hook, `SOT_SVG_CHILD_STRAY`): the replies still parse whole, and the line is nowhere in them.
#[cfg(debug_assertions)]
#[test]
fn a_library_print_never_reaches_the_reply_stream() {
    let (red, blue) = (rect("red"), rect("blue"));
    let input = request(&[(4, 4, red.as_bytes()), (6, 2, b"this is no SVG"), (4, 4, blue.as_bytes())]);
    let ran = run_with_env(WAIT, ROOMY, 30, input, &[("SOT_SVG_CHILD_STRAY", "1")]);
    assert_eq!(ran.status.code(), Some(0));
    assert!(!String::from_utf8_lossy(&ran.out).contains("Warning"), "the stray line reached the reply stream");
    let replies = replies(&ran.out);
    assert_eq!(replies.len(), 3, "one reply per job");
    assert_eq!(pixel(&replies[0], 2, 2), [255, 0, 0, 255]);
    assert!(matches!(&replies[1], Reply::Error(_)));
    assert_eq!(pixel(&replies[2], 2, 2), [0, 0, 255, 255]);
}

#[test]
fn an_error_text_is_cut_at_a_kilobyte() {
    // An unknown entity named by 3000 characters: the parser's message carries the name.
    let svg = format!("{SVG_OPEN}<rect width=\"10\" height=\"10\" id=\"&{};\"/></svg>", "A".repeat(3000));
    let ran = run(ROOMY, 30, request(&[(2, 2, svg.as_bytes())]));
    assert_eq!(ran.status.code(), Some(0));
    let replies = replies(&ran.out);
    let [Reply::Error(text)] = &replies[..] else { panic!("one error: {replies:?}") };
    assert_eq!(text.len(), ERROR_BYTES, "cut at the limit");
}

/// 100 million octaves of noise: a few hundred bytes that spin for weeks.
const SPIN: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100"><filter id="f">
<feTurbulence baseFrequency="0.05" numOctaves="100000000"/></filter><rect width="100" height="100" filter="url(#f)"/></svg>"##;

/// In a debug build, without the bound, this file ends by itself after about 6 s: resvg's `debug_assert!(val.is_finite())`
/// (filter/mod.rs:245) panics, and a panic on the draw thread is `EXIT_CRASH`, not `EXIT_CPU`.
#[cfg(unix)]
#[test]
fn a_child_past_its_cpu_bound_ends_itself() {
    let ran = run_within(Duration::from_secs(10), ROOMY, 1, request(&[(100, 100, SPIN.as_bytes())]));
    assert_eq!(ran.status.code(), Some(EXIT_CPU), "the CPU bound is an exit with its code, not SIGXCPU");
    assert!(ran.lived < Duration::from_secs(10), "{:?}", ran.lived);
}

/// A panic on the draw thread is `EXIT_CRASH` on every system, not the abort's status (on Windows a fast-fail). Only a
/// debug build has a file known to panic: resvg's `debug_assert!(val.is_finite())` stops `SPIN` after about 6 s. On
/// Unix an abort would give the same code, so this is red there only for another code; on Windows it is red for `abort`.
#[cfg(debug_assertions)]
#[test]
fn a_panic_on_the_draw_thread_is_the_crash_code() {
    let ran = run_within(Duration::from_secs(30), ROOMY, 60, request(&[(100, 100, SPIN.as_bytes())]));
    assert_eq!(ran.status.code(), Some(EXIT_CRASH));
    assert_eq!(replies(&ran.out).len(), 0, "nothing was answered before the panic");
}

// The runner. The real child as the window starts it, and stand-ins for it (Unix: `/bin/sh` with absolute inner
// commands, since the runner's environment has no `PATH`).

fn job(w: u32, h: u32, svg: &[u8]) -> Job {
    Job { ask: Ask::Stretch(w, h), svg: Arc::from(svg) }
}

fn refused(words: &str) -> Outcome {
    Outcome::Refused(words.to_owned())
}

fn ended(words: &str) -> Outcome {
    Outcome::Ended(words.to_owned())
}

/// The shell script that starts the real child under the core limit with `HOME` in `scratch`.
#[cfg(unix)]
fn wrap(scratch: &Scratch) -> String {
    format!("HOME='{}'; export HOME; {WRAP}", scratch.0.display())
}

/// The real child with `heap` bytes of heap and a wall of `wall`; on Unix under a core limit, in `scratch`.
fn real_child(wall: Duration, heap: usize, scratch: &Scratch) -> (Command, Bounds) {
    let bounds = Bounds { wall, heap: heap as u64, cpu: 30 };
    let bin = env!("CARGO_BIN_EXE_sot");
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &wrap(scratch), bin]).args(bounds.args());
        command
    };
    #[cfg(not(unix))]
    let mut command = svg_run::child_command(bin, &bounds);
    command.current_dir(&scratch.0);
    (command, bounds)
}

/// A stand-in child: `script` under `/bin/sh`, ignoring its input.
#[cfg(unix)]
fn stand_in(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]);
    command
}

#[cfg(unix)]
fn process_gone(pid: u32) -> bool {
    !sot_platform::test_wait::run_within(Command::new("/bin/kill").args(["-0", &pid.to_string()]), WAIT).status.success()
}

#[cfg(windows)]
fn process_gone(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: a handle opened here is closed here; the call only reads a status.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return true;
        }
        let mut code = 0;
        let read = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        read != 0 && code != STILL_ACTIVE as u32
    }
}

#[test]
fn the_runner_returns_each_job_as_its_pixels_or_its_error() {
    let (red, blue) = (rect("red"), rect("blue"));
    let scratch = Scratch::new();
    let (command, bounds) = real_child(WAIT, ROOMY, &scratch);
    let jobs = vec![job(4, 4, red.as_bytes()), job(6, 2, b"this is no SVG"), job(0, 3, blue.as_bytes())];
    let outcomes = draw_in_child(command, jobs, &bounds, &Stop::default());
    assert_eq!(outcomes.len(), 3);
    assert!(matches!(&outcomes[0], Outcome::Drawn { w: 4, h: 4, rgba } if rgba[4 * (2 * 4 + 2)..][..4] == [255, 0, 0, 255]));
    assert!(matches!(&outcomes[1], Outcome::Refused(text) if text.starts_with("svg parse failed")), "{:?}", outcomes[1]);
    // A zero side is drawn as one, and the runner accepts that as the ask.
    assert!(matches!(&outcomes[2], Outcome::Drawn { w: 1, h: 3, .. }), "{:?}", outcomes[2]);
}

#[test]
fn a_program_that_cannot_be_started_is_refused_in_words() {
    let outcomes = draw_in_child(Command::new("/no/such/program"), vec![job(2, 2, b"<svg/>"), job(2, 2, b"<svg/>")], &Bounds {
        wall: WAIT,
        heap: ROOMY as u64,
        cpu: 30,
    }, &Stop::default());
    assert_eq!(outcomes, [refused("cannot start the drawing"), refused("cannot start the drawing")]);
}

#[test]
fn a_draw_past_its_wall_bound_is_killed_and_refused() {
    let scratch = Scratch::new();
    let (command, bounds) = real_child(Duration::from_secs(1), ROOMY, &scratch);
    let started = start(command).expect("start the child");
    let pid = started.child.id();
    let begun = Instant::now();
    let outcomes = finish(started, vec![job(100, 100, SPIN.as_bytes())], &bounds, &Stop::default());
    assert_eq!(outcomes, [ended("took past 1 s")]);
    assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
    assert!(process_gone(pid), "the child is gone once the runner returns");
}

#[test]
fn a_child_that_ends_early_refuses_the_rest_of_its_batch_in_its_words() {
    // A batch is one child: the job that ends it, and the jobs after it, never begun, are refused in the words of its
    // end; the one before it was answered whole. It ends at its heap, not at the wall: a hosted Windows child's cold
    // first start (the binary and the system fonts) takes about 3 s, so any wall short of `WAIT` would end the child
    // before its first job is drawn and say the wall's words, which is not what this test is about.
    let mut bomb = format!("<!DOCTYPE svg [<!ENTITY a \"{}\">]>{SVG_OPEN}<rect width=\"10\" height=\"10\" id=\"", "x".repeat(MIB));
    bomb.push_str(&"&a;".repeat(200));
    bomb.push_str("\"/></svg>");
    let red = rect("red");
    let scratch = Scratch::new();
    let (command, bounds) = real_child(WAIT, 64 * MIB, &scratch);
    let jobs = vec![job(4, 4, red.as_bytes()), job(2, 2, bomb.as_bytes()), job(4, 4, red.as_bytes()), job(4, 4, red.as_bytes())];
    let outcomes = draw_in_child(command, jobs, &bounds, &Stop::default());
    assert_eq!(outcomes.len(), 4);
    assert!(matches!(&outcomes[0], Outcome::Drawn { w: 4, h: 4, .. }), "{:?}", outcomes[0]);
    assert_eq!(outcomes[1..], [ended("needed more than 64 MiB"), ended("needed more than 64 MiB"), ended("needed more than 64 MiB")]);
}

#[test]
fn a_heap_end_during_the_input_write_says_heap() {
    // 200 MiB against a 64 MiB heap: the child ends while its input is still being written, so the writer meets a
    // closed pipe, and the words are the exit status's.
    let svg = vec![b' '; 200 * MIB];
    let scratch = Scratch::new();
    let (command, bounds) = real_child(WAIT, 64 * MIB, &scratch);
    assert_eq!(draw_in_child(command, vec![job(2, 2, &svg)], &bounds, &Stop::default()), [ended("needed more than 64 MiB")]);
}

#[test]
fn the_childs_environment_is_the_font_list() {
    let os = |names: &[&str]| names.iter().map(|name| (name.into(), "x".into())).collect::<Vec<(std::ffi::OsString, std::ffi::OsString)>>();
    let kept = child_env(os(&["SOT_TOKEN", "DISPLAY", "HOME", "USERPROFILE", "SystemRoot", "XDG_DATA_HOME", "PATH"]).into_iter());
    let kept: Vec<_> = kept.iter().map(|(name, _)| name.to_str().unwrap()).collect();
    if cfg!(windows) {
        assert_eq!(kept, ["USERPROFILE", "SystemRoot"], "Windows spells them in any case");
    } else {
        assert_eq!(kept, ["HOME", "XDG_DATA_HOME"]);
    }
}

#[cfg(unix)]
#[test]
fn a_binary_without_the_mode_is_refused_at_once() {
    // It prints its version line, reads nothing and sleeps: the refusal comes at the first line, though the input is
    // far past what a pipe holds (so a runner that writes before it reads waits for the wall).
    let (begun, svg) = (Instant::now(), vec![b' '; MIB]);
    let command = stand_in("/usr/bin/printf 'sot 0.7.0\\n'; exec /bin/sleep 30");
    let bounds = Bounds { wall: Duration::from_secs(20), heap: ROOMY as u64, cpu: 30 };
    assert_eq!(draw_in_child(command, vec![job(2, 2, &svg)], &bounds, &Stop::default()), [refused("not a drawing program")]);
    assert!(begun.elapsed() < Duration::from_secs(5), "{:?}", begun.elapsed());
}

/// A stand-in child's whole reply to a 1x1 job: the magic and a red pixel.
#[cfg(unix)]
const ONE_RED_PIXEL: &str = r"SVG1R\001\000\000\000\001\000\000\000\377\000\000\377";

#[cfg(unix)]
#[test]
fn bytes_after_the_last_reply_are_refused() {
    let bounds = Bounds { wall: WAIT, heap: ROOMY as u64, cpu: 30 };
    let ask = |script: String| draw_in_child(stand_in(&script), vec![job(1, 1, b"<svg/>")], &bounds, &Stop::default());
    // The control: the reply alone is drawn.
    assert_eq!(ask(format!("/usr/bin/printf '{ONE_RED_PIXEL}'")), [Outcome::Drawn { w: 1, h: 1, rgba: vec![255, 0, 0, 255] }]);
    assert_eq!(ask(format!("/usr/bin/printf '{ONE_RED_PIXEL}X'")), [refused(BAD_REPLY)]);
}

#[cfg(unix)]
#[test]
fn a_reply_past_its_ask_is_refused() {
    // The header names 5000 x 5000 for a 2 x 2 ask, then the child waits: it is refused at the header, not at the wall
    // and not after 100 MB of body.
    let begun = Instant::now();
    let command = stand_in(r"/usr/bin/printf 'SVG1R\210\023\000\000\210\023\000\000'; exec /bin/sleep 30");
    let bounds = Bounds { wall: Duration::from_secs(5), heap: ROOMY as u64, cpu: 30 };
    assert_eq!(draw_in_child(command, vec![job(2, 2, b"<svg/>")], &bounds, &Stop::default()), [refused(BAD_REPLY)]);
    assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
}

#[test]
fn the_runner_returns_a_fit_ask_at_the_childs_size() {
    let scratch = Scratch::new();
    let (command, bounds) = real_child(WAIT, ROOMY, &scratch);
    let jobs = vec![Job { ask: Ask::Fit { pane: (400, 200), cap: 16384 }, svg: Arc::from(rect("red").as_bytes()) }];
    let outcomes = draw_in_child(command, jobs, &bounds, &Stop::default());
    assert!(matches!(&outcomes[0], Outcome::Drawn { w: 200, h: 200, .. }), "{:?}", outcomes[0]);
}

#[cfg(unix)]
#[test]
fn a_fit_reply_past_its_cap_or_the_pixel_budget_is_refused_at_its_header() {
    // The headers name 101 by 1 under a cap of 100, and 5000 by 5000 (25 Mpx) under a cap of 16384; then the child
    // waits, so a refusal at the wall or after the body would be slow. The control is within both.
    let bounds = Bounds { wall: Duration::from_secs(5), heap: ROOMY as u64, cpu: 30 };
    let ask = |cap: u32, header: &str| {
        let command = stand_in(&format!("/usr/bin/printf 'SVG1R{header}'; exec /bin/sleep 30"));
        let jobs = vec![Job { ask: Ask::Fit { pane: (100, 100), cap }, svg: Arc::from(&b"<svg/>"[..]) }];
        let begun = Instant::now();
        let outcomes = draw_in_child(command, jobs, &bounds, &Stop::default());
        assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
        outcomes
    };
    assert_eq!(ask(100, r"\145\000\000\000\001\000\000\000"), [refused(BAD_REPLY)]);
    assert_eq!(ask(16384, r"\210\023\000\000\210\023\000\000"), [refused(BAD_REPLY)]);
    // Within both limits it is read as drawn (the body is one pixel, so the child's sleep is cut short by the end).
    let command = stand_in(&format!("/usr/bin/printf '{ONE_RED_PIXEL}'"));
    let jobs = vec![Job { ask: Ask::Fit { pane: (100, 100), cap: 100 }, svg: Arc::from(&b"<svg/>"[..]) }];
    assert_eq!(draw_in_child(command, jobs, &bounds, &Stop::default()), [Outcome::Drawn { w: 1, h: 1, rgba: vec![255, 0, 0, 255] }]);
}

#[cfg(unix)]
#[test]
fn a_reply_that_breaks_its_form_is_refused() {
    let bounds = Bounds { wall: WAIT, heap: ROOMY as u64, cpu: 30 };
    for (what, reply) in [
        ("an error text past 1 KiB", r"SVG1E\001\004"),
        ("an error text that is not UTF-8", r"SVG1E\002\000\377\376"),
        ("a tag the runner does not know", "SVG1Z"),
    ] {
        let command = stand_in(&format!("/usr/bin/printf '{reply}'"));
        assert_eq!(draw_in_child(command, vec![job(1, 1, b"<svg/>")], &bounds, &Stop::default()), [refused(BAD_REPLY)], "{what}");
    }
}

#[cfg(unix)]
#[test]
fn a_child_the_runner_has_to_kill_is_refused_in_the_walls_words() {
    // It closes its stdout and lives: the runner ends it, and says what the wall says, not that a signal ended it.
    let begun = Instant::now();
    let command = stand_in("/usr/bin/printf 'SVG1'; exec /bin/sleep 30 >&-");
    let bounds = Bounds { wall: Duration::from_secs(5), heap: ROOMY as u64, cpu: 30 };
    assert_eq!(draw_in_child(command, vec![job(2, 2, b"<svg/>")], &bounds, &Stop::default()), [ended("took past 5 s")]);
    assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
}

#[cfg(unix)]
#[test]
fn a_stopped_draw_is_killed_at_once_and_stale() {
    // A child that answers nothing and lives: the stop, not the wall, ends it, whether it comes while the runner waits
    // or before the wait begins.
    let bounds = Bounds { wall: Duration::from_secs(20), heap: ROOMY as u64, cpu: 30 };
    for early in [false, true] {
        let started = start(stand_in("/usr/bin/printf 'SVG1'; exec /bin/sleep 30")).expect("start the child");
        let pid = started.child.id();
        let stop = Stop::default();
        let from_elsewhere = stop.clone();
        let wait = Duration::from_millis(if early { 0 } else { 300 });
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(wait);
            from_elsewhere.stop();
        });
        if early {
            stopper.join().unwrap();
        }
        let begun = Instant::now();
        let outcomes = finish(started, vec![job(2, 2, b"<svg/>"), job(2, 2, b"<svg/>")], &bounds, &stop);
        assert_eq!(outcomes, [Outcome::Stale, Outcome::Stale], "early: {early}");
        assert!(begun.elapsed() < Duration::from_secs(3), "{:?}", begun.elapsed());
        assert!(process_gone(pid), "the child is gone once the runner returns");
    }
}

#[cfg(unix)]
#[test]
fn a_dropped_start_ends_its_child() {
    let started = start(stand_in("exec /bin/sleep 30")).expect("start the child");
    let pid = started.child.id();
    drop(started);
    assert!(process_gone(pid), "dropping the start killed and reaped the child");
}

/// A program that lives for 30 s, and one that ends at once with code 74, on each system.
fn long_and_short() -> (Command, Command) {
    #[cfg(unix)]
    let ((long, long_args), (short, short_args)) = (("/bin/sleep", ["30"].as_slice()), ("/bin/sh", ["-c", "exit 74"].as_slice()));
    #[cfg(windows)]
    let ((long, long_args), (short, short_args)) = (("ping", ["-n", "30", "127.0.0.1"].as_slice()), ("cmd", ["/c", "exit", "74"].as_slice()));
    let mut commands = (Command::new(long), Command::new(short));
    commands.0.args(long_args);
    commands.1.args(short_args);
    commands
}

#[test]
fn the_runners_own_kill_is_known_by_its_status() {
    let (mut long, mut short) = long_and_short();
    let spawn = |command: &mut Command| command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let mut child = spawn(&mut long);
    child.kill().unwrap();
    assert!(killed_by_us(&child.wait().unwrap()), "a killed child");
    assert!(!killed_by_us(&spawn(&mut short).wait().unwrap()), "a child that ended by its own code");
}

#[cfg(target_os = "linux")]
#[test]
fn a_child_reads_no_descriptor_of_the_window() {
    // A descriptor the window left open without close-on-exec, which the shell stands in for by opening one on its way
    // to `exec`. The child is handed a request that spins and its magic is awaited, so that it has bounded itself and
    // is drawing when its descriptors are read, which also holds what `start` applied: the child's descriptor 2 is the
    // null device, and its environment is the font list (a token the test sets does not arrive).
    let scratch = Scratch::new();
    let held = scratch.0.join("held");
    std::fs::write(&held, b"x").unwrap();
    let bounds = Bounds { wall: WAIT, heap: ROOMY as u64, cpu: 30 };
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", &format!("{} 3<'{}'", wrap(&scratch), held.display()), env!("CARGO_BIN_EXE_sot")])
        .args(bounds.args())
        .env("SOT_SVG_TEST_TOKEN", "secret")
        .current_dir(&scratch.0);
    let mut started = start(command).expect("start the child");
    let mut stdin = started.child.stdin.take().unwrap();
    stdin.write_all(&request(&[(100, 100, SPIN.as_bytes())])).unwrap();
    let mut magic = [0; 4];
    started.child.stdout.as_mut().unwrap().read_exact(&mut magic).unwrap();
    assert_eq!(magic, MAGIC);
    let pid = started.child.id();
    // The child closes the fonts it loads while it is drawn, so an entry can vanish between the listing and its read.
    let held_by_child: Vec<_> = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .unwrap()
        .filter_map(|entry| match std::fs::read_link(entry.unwrap().path()) {
            Ok(link) => Some(link),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("{error}"),
        })
        .collect();
    let stderr = std::fs::read_link(format!("/proc/{pid}/fd/2")).unwrap();
    let environ = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    started.child.kill().unwrap();
    started.child.wait().unwrap();
    assert!(held_by_child.len() >= 3, "the listing reads the child's descriptors: {held_by_child:?}");
    assert!(!held_by_child.contains(&held), "the window's descriptor is open in the child: {held_by_child:?}");
    assert_eq!(stderr, std::path::Path::new("/dev/null"), "the child's stderr is the null device");
    // The shell adds a few variables of its own (bash exports these) before it execs the child.
    let names = environ.split(|byte| *byte == 0).filter(|var| !var.is_empty()).map(|var| String::from_utf8_lossy(var.split(|b| *b == b'=').next().unwrap()).into_owned());
    for name in names.filter(|name| !["PWD", "OLDPWD", "SHLVL", "_"].contains(&name.as_str())) {
        assert_eq!(child_env([(name.clone().into(), "".into())].into_iter()).len(), 1, "{name} is not a font variable");
    }
}

#[cfg(windows)]
#[test]
fn a_started_child_is_in_its_job_before_it_is_given_any_input() {
    let scratch = Scratch::new();
    let (command, _) = real_child(WAIT, ROOMY, &scratch);
    let mut started = start(command).expect("start the child");
    // The pipes are still unused here, so no input byte has been written.
    assert_eq!(started.job.active_processes().unwrap(), 1, "the child is in the job");
    started.child.kill().unwrap();
    started.child.wait().unwrap();
}
