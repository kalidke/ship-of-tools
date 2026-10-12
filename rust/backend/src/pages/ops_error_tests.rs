//! Direct diagnostic tables and one isolated real-handler/writer boundary.

use super::quarto_render_error;
use sot_protocol::{codec, Frame};

const REQUEST_ID: u64 = 407;
const LIMIT: usize = 65_536;
const TRANSPORT_ERROR: &str = "Read invalid transport file that did not end with a newline";
const HINT: &str = "Quarto read an incomplete Julia server transport line. Quarto 1.9.38 and later wait up to two seconds for the newline, so slow server startup can still cause this error. If using an older Quarto, upgrading to 1.9.38 or later may help.";

fn diagnostic_frame(message: &str) -> Frame {
    Frame::res(REQUEST_ID, sot_protocol::op::QUARTO_OPEN,
        serde_json::json!({"error": message, "code": "quarto_render_failed"}))
}

fn assert_bounds(message: &str, case: &str) {
    assert!(message.len() <= LIMIT,
        "Quarto diagnostics exceed the 65536-byte message bound: case={case} bytes={}", message.len());
    let frame = diagnostic_frame(message);
    let bytes = serde_json::to_vec(&frame).unwrap().len();
    assert!(bytes <= codec::MAX_ENVELOPE_BYTES, "case={case} JSON bytes={bytes}");
}

fn stack_tail() -> String {
    (0..10).map(|n| format!("stack frame {n:02}")).collect::<Vec<_>>().join("\n")
}

fn error_flood() -> Vec<u8> {
    let mut stderr = String::from("ERROR root cause kept\n");
    for n in 0..12_000 {
        stderr.push_str(&format!("ERROR flood {n:05} {}\n", "x".repeat(90)));
    }
    stderr.push_str(&format!("ERROR: {TRANSPORT_ERROR}\n"));
    assert!(stderr.len() > 1_048_576, "ERROR fixture must exceed 1 MiB");
    stderr.push_str(&stack_tail());
    stderr.push('\n');
    stderr.into_bytes()
}

fn assert_flood(message: &str) {
    assert_bounds(message, "numbered flood");
    assert_eq!(message.matches("ERROR root cause kept").count(), 1);
    assert!(message.ends_with(&format!("{}\n{HINT}", stack_tail())));
    assert_eq!(message.matches(HINT).count(), 1);
    assert!(!message.contains(TRANSPORT_ERROR), "trigger must be beyond optional budget");
    let kept: Vec<_> = message.lines().filter(|line| line.starts_with("ERROR flood ")).collect();
    assert!(!kept.is_empty() && kept.len() < 12_000);
    for (n, line) in kept.iter().enumerate() {
        assert_eq!(*line, format!("ERROR flood {n:05} {}", "x".repeat(90)));
    }
    let marker = format!("[{} more ERROR lines omitted]", 12_001 - kept.len());
    assert_eq!(message.matches(&marker).count(), 1);
    assert_eq!(message.matches("more ERROR lines omitted]").count(), 1);
    assert!(message.contains(&format!("{marker}\n{}", stack_tail())));
    for n in 0..10 {
        assert_eq!(message.matches(&format!("stack frame {n:02}")).count(), 1);
    }
}

#[test]
fn quarto_render_error_selection_table() {
    let tail = stack_tail();
    let mut cases: Vec<(&str, Vec<u8>, String)> = vec![
        ("empty", b"".to_vec(), String::new()),
        ("whitespace", b" \t\n\r\n".to_vec(), String::new()),
        ("short", b"short stderr".to_vec(), ":\nshort stderr".into()),
        ("CRLF", b"first\r\nsecond\r\n".to_vec(), ":\nfirst\nsecond".into()),
        ("trailing newline", b"trailing newline\n".to_vec(), ":\ntrailing newline".into()),
        ("lossy", b"ERROR invalid \xff\nframe\n".to_vec(), ":\nERROR invalid \u{fffd}\nframe".into()),
        ("tail overlap", b"ERROR tail\nordinary\n".to_vec(), ":\nERROR tail\nordinary".into()),
    ];
    cases.push(("early error", format!("ERROR early\nordinary omitted\n{tail}\n").into_bytes(),
        format!(":\nERROR early\n{tail}")));
    cases.push(("ordinary tail", format!("ordinary omitted\n{tail}\n").into_bytes(), format!(":\n{tail}")));
    let color = "\u{1b}[31mERROR colored\u{1b}[0m";
    cases.push(("repeats and ANSI", format!("ERROR repeated\nordinary omitted\n{color}\nERROR repeated\n{tail}\n").into_bytes(),
        format!(":\nERROR repeated\n{color}\nERROR repeated\n{tail}")));
    for (case, stderr, suffix) in cases {
        for execute in [false, true] {
            assert_eq!(quarto_render_error(execute, &stderr),
                format!("quarto render failed (execute={execute}){suffix}"), "case={case}");
        }
    }
}

fn check_optional_boundary(execute: bool) {
    let tail = stack_tail();
    let prefix = format!("quarto render failed (execute={execute})");
    let mandatory = format!("ERROR first\n{tail}");
    let optional_len = LIMIT - prefix.len() - 2 - mandatory.len() - 1;
    let optional = format!("ERROR optional {}",
        "x".repeat(optional_len - "ERROR optional ".len()));
    let stderr = format!("ERROR first\n{optional}\n{tail}");
    let message = quarto_render_error(execute, stderr.as_bytes());
    let expected = format!("{prefix}:\n{stderr}");
    assert!(message == expected,
        "a complete selection that fits must not reserve an unused omission marker: expected-bytes={} actual-bytes={}",
        expected.len(), message.len());
    assert_eq!(message.len(), LIMIT);
    assert_bounds(&message, "complete selection exact fit");
    let base = quarto_render_error(execute, format!("ERROR first\n{tail}").as_bytes());
    // Three optional occurrences: fitting candidate, nonfit, then small. Reserve their marker and separators.
    let size = LIMIT - base.len() - "[3 more ERROR lines omitted]".len() - 2;
    for extra in [0, 1] {
        let optional = format!("ERROR exact {} END", "x".repeat(size + extra - "ERROR exact  END".len()));
        let oversized = format!("ERROR oversized {} END", "x".repeat(LIMIT));
        let stderr = format!("ERROR first\n{optional}\n{oversized}\nERROR small\n{tail}");
        let message = quarto_render_error(execute, stderr.as_bytes());
        assert_bounds(&message, "optional boundary");
        assert_eq!(message.contains(&optional), extra == 0, "exact fit versus first nonfit");
        assert!(!message.contains("ERROR oversized") && !message.contains("ERROR small"), "stop rather than skip");
        assert!(message.ends_with(&format!("[{} more ERROR lines omitted]\n{tail}", 2 + extra)));
        if extra == 0 { assert_eq!(message.len(), LIMIT); }
    }
}

fn check_bound_corners(execute: bool) {
    let tail = stack_tail();
    let trigger = format!("ordinary {TRANSPORT_ERROR}\n{tail}");
    let message = quarto_render_error(execute, trigger.as_bytes());
    assert_bounds(&message, "unselected transport trigger");
    assert_eq!(message, format!("quarto render failed (execute={execute}):\n{tail}\n{HINT}"));
    let message = quarto_render_error(execute, format!("{}\nERROR first in tail\n{tail}", "ordinary\n".repeat(15)).as_bytes());
    // The first ERROR is immediately outside this tail; also exercise one actually inside it.
    assert_eq!(message.matches("ERROR first in tail").count(), 1);
    let stderr = format!("{}\nERROR first inside tail\nend", "ordinary\n".repeat(15));
    let message = quarto_render_error(execute, stderr.as_bytes());
    assert_eq!(message.matches("ERROR first inside tail").count(), 1);
    assert!(!message.contains("more ERROR lines omitted]"));
    let stderr = format!("{}\n{tail}", "ERROR repeated\n".repeat(12_000));
    let message = quarto_render_error(execute, stderr.as_bytes());
    assert_bounds(&message, "repeated occurrences");
    let kept = message.lines().filter(|line| *line == "ERROR repeated").count();
    assert!(kept > 1 && kept < 12_000);
    assert!(message.contains(&format!("[{} more ERROR lines omitted]", 12_000 - kept)));
    let stderr = format!("ERROR {}\n{tail}", "\u{1b}\u{1}\t\"\\".repeat(20_000));
    let message = quarto_render_error(execute, stderr.as_bytes());
    assert_bounds(&message, "ANSI and JSON control expansion");
    assert!(message.contains("[line truncated]") && message.contains(&tail));
}

fn check_mandatory_excerpts(execute: bool) {
    let mut stderr = format!("ERROR FIRST {} FIRST-END\n", "界🙂".repeat(30_000));
    for n in 0..10 { stderr.push_str(&format!("TAIL-{n:02} {} END-{n:02}\n", "界🙂".repeat(30_000))); }
    let message = quarto_render_error(execute, stderr.as_bytes());
    assert_bounds(&message, "huge UTF-8 mandatory lines");
    assert_eq!(message.matches("[line truncated]").count(), 11);
    assert_eq!(message.matches("ERROR FIRST").count(), 1);
    assert!(message.contains("FIRST-END") && !message.contains('\u{fffd}'));
    assert!(!message.contains("more ERROR lines omitted]"));
    for n in 0..10 {
        assert_eq!(message.matches(&format!("TAIL-{n:02}")).count(), 1);
        assert_eq!(message.matches(&format!("END-{n:02}")).count(), 1);
    }
    let mut bytes = format!("ERROR invalid {}", "x".repeat(80_000)).into_bytes();
    bytes.extend_from_slice(b"\xff END\n");
    bytes.extend_from_slice(stack_tail().as_bytes());
    let message = quarto_render_error(execute, &bytes);
    assert_bounds(&message, "lossy mandatory clipping");
    assert!(message.contains("[line truncated]") && message.contains("\u{fffd} END"));
    assert!(message.ends_with(&stack_tail()));
    let stderr = format!("ERROR short\n{}\n{}", "ordinary\n".repeat(15),
        (0..10).map(|n| if n == 4 { format!("ERROR tail-{n:02} {} END", "x".repeat(90_000)) }
            else { format!("tail-{n:02}") }).collect::<Vec<_>>().join("\n"));
    let message = quarto_render_error(execute, stderr.as_bytes());
    assert_bounds(&message, "short mandatory lines survive oversized peer");
    assert!(message.contains("ERROR short\n") && message.contains("ERROR tail-04"));
    assert_eq!(message.matches("[line truncated]").count(), 1);
    for n in [0, 1, 2, 3, 5, 6, 7, 8, 9] { assert!(message.contains(&format!("tail-{n:02}"))); }
}

#[test]
fn quarto_render_error_bound_table() {
    for execute in [false, true] {
        // At the parent this first assertion fails before checking the new guidance.
        let message = quarto_render_error(execute, &error_flood());
        assert_bounds(&message, "numbered flood");
        assert_flood(&message);
        check_optional_boundary(execute);
        check_bound_corners(execute);
        check_mandatory_excerpts(execute);
    }
    println!("quarto-bound table: message=within-65536 first-error=present tail=preserved errors=ordered omission=exact utf8=ok frame=within-cap");
}

#[test]
fn quarto_render_error_hint_table() {
    let tail = stack_tail();
    for execute in [false, true] {
        for (case, stderr) in [
            ("selected trigger", format!("ERROR: {TRANSPORT_ERROR}\n{tail}\n").into_bytes()),
            ("unselected trigger", format!("ordinary {TRANSPORT_ERROR}\nignored ordinary\n{tail}\n").into_bytes()),
            ("blank selected tail", format!("{TRANSPORT_ERROR}\n{}", "\n".repeat(10)).into_bytes()),
        ] {
            let message = quarto_render_error(execute, &stderr);
            assert!(message.contains(HINT),
                "transport hint missing: case={case} execute={execute}");
            assert_eq!(message.matches(HINT).count(), 1);
            assert!(message.ends_with(HINT));
            assert_bounds(&message, case);
            if case == "blank selected tail" {
                assert_eq!(message, format!("quarto render failed (execute={execute})\n{HINT}"));
            }
        }
        for stderr in ["ERROR unrelated failure\nstack frame\n", "ordinary failure\n",
            "Read invalid transport file that did not end with a newlin\n",
            "read invalid transport file that did not end with a newline\n"] {
            let message = quarto_render_error(execute, stderr.as_bytes());
            assert_eq!(message, format!("quarto render failed (execute={execute}):\n{}", stderr.trim_end_matches('\n')));
            assert!(!message.contains(HINT) && !message.contains("1.9.38"));
        }
    }
    println!("quarto-hint table: execute=both newer-wait=2s slow-start=explained upgrade=conditional controls=absent");
}

#[cfg(unix)]
mod handler {
    use super::*;
    use crate::pages::ops::{handle_quarto_open, Session};
    use crate::paths::EnvGuard;
    use std::time::Duration;

    fn install_stand_ins(dir: &std::path::Path, stderr: &[u8]) {
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        sot_log::test_exec::write_executable(&bin.join("julia"), "#!/bin/sh\nexit 0\n");
        std::fs::write(dir.join("stderr.bin"), stderr).unwrap();
        let body = r#"#!/bin/sh
set -eu
printf 'render\n' >> invocations
out=''
while [ "$#" -gt 0 ]; do
    if [ "$1" = '--output' ]; then
        shift
        out=$1
    fi
    shift
done
test -n "$out"
printf 'partial HTML\n' > "$out"
cat stderr.bin >&2
printf 'stderr-written\n' > completed
exit 1
"#;
        sot_log::test_exec::write_executable(&bin.join("quarto"), body);
        std::fs::write(dir.join("doc.qmd"), "---\nengine: julia\n---\n").unwrap();
        let mut paths = vec![bin.clone()];
        if let Some(preserved) = std::env::var_os("PATH") { paths.extend(std::env::split_paths(&preserved)); }
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        std::env::set_var("SOT_JULIA_BIN", bin.join("julia"));
        std::env::remove_var("QUARTO_JULIA");
    }

    fn failed_render(stderr: &[u8], execute: bool) -> Frame {
        let _serial = crate::paths::ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _restore = [EnvGuard::capture("PATH"), EnvGuard::capture("SOT_JULIA_BIN"), EnvGuard::capture("QUARTO_JULIA")];
        let dir = tempfile::tempdir().unwrap();
        assert!(dir.path().is_absolute());
        install_stand_ins(dir.path(), stderr);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let session = Session::new();
        let replies = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), handle_quarto_open(REQUEST_ID,
                serde_json::json!({"path": dir.path().join("doc.qmd"), "execute": execute}), &session)).await
        }).unwrap_or_else(|error| panic!(
            "the handler exceeded its five-second bound: {error}; stub entered={}, stderr written={}",
            dir.path().join("invocations").exists(), dir.path().join("completed").exists(),
        )).expect("handler reply");
        assert_eq!(replies.len(), 1, "one failure reply");
        let (frame, blob) = &replies[0];
        assert_eq!(frame.id, REQUEST_ID);
        assert_eq!(frame.op, sot_protocol::op::QUARTO_OPEN);
        assert_eq!(frame.kind, sot_protocol::Kind::Res);
        assert_eq!(frame.payload["code"], "quarto_render_failed");
        assert!(blob.is_none(), "a failed render carries no blob");
        assert_eq!(std::fs::read_to_string(dir.path().join("invocations")).unwrap(), "render\n", "render once");
        assert!(!dir.path().join(format!("__sot-qmd-{REQUEST_ID}.html")).exists(), "partial HTML removed");
        frame.clone()
    }

    fn check_writer(frame: &Frame) -> bool {
        let json_bytes = serde_json::to_vec(frame).unwrap().len();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let mut sink = Vec::new();
            if let Err(error) = crate::server::reply::write_frame_to(&mut sink, frame, None).await {
                let typed = error.downcast_ref::<codec::EnvelopeTooLarge>().expect("typed writer cap rejection");
                assert_eq!(typed.len, json_bytes);
                assert!(json_bytes > codec::MAX_ENVELOPE_BYTES && sink.is_empty());
                println!("quarto-handler red: json=over-cap writer=EnvelopeTooLarge bytes-written=0");
                return false;
            }
            assert!(json_bytes <= codec::MAX_ENVELOPE_BYTES);
            assert_flood(frame.payload["error"].as_str().unwrap());
            let next = Frame::res(REQUEST_ID + 1, "ping", serde_json::json!({"ok": true}));
            crate::server::reply::write_frame_to(&mut sink, &next, None).await.unwrap();
            let mut reader = std::io::Cursor::new(sink);
            for expected in [frame, &next] {
                let (actual, blob) = codec::read_frame(&mut reader).await.unwrap();
                assert_eq!(actual.id, expected.id);
                assert_eq!(actual.op, expected.op);
                assert_eq!(actual.kind, expected.kind);
                assert_eq!(actual.payload, expected.payload);
                assert!(blob.is_none());
            }
            assert_eq!(reader.position() as usize, reader.get_ref().len());
            true
        })
    }

    #[test]
    fn quarto_failure_reply_survives_error_flood() {
        if !sot_log::test_isolated::run_isolated(
            "pages::ops::error_tests::handler::quarto_failure_reply_survives_error_flood",
        ) { return; }
        crate::lifecycle::child_signal::reset_child_signal();
        let mut accepted = true;
        for execute in [false, true] {
            let frame = failed_render(&error_flood(), execute);
            accepted &= check_writer(&frame);
        }
        assert!(accepted, "large Quarto diagnostics were rejected at the server writer");
        println!("quarto-handler probe: execute=both code=quarto_render_failed hint=present first-error=present tail=10 omitted=exact renders-per-call=1 partial-html=absent frame=within-cap writer=accepted next-frame=ok");
    }
}
