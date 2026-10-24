use super::*;
use std::io::Read;

const MARK: &str = "<redacted>";

fn red(s: &str) -> String {
    String::from_utf8(redact(s.as_bytes()).into_owned()).unwrap()
}

/// A token as the daemon's `random_token` mints it: 16 bytes from the OS, 32 lowercase hex characters.
fn minted() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).unwrap();
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// ADR 0049, User isolation: a secret an event carries never reaches the file, through the real fmt layer.
#[test]
fn a_logged_secret_is_masked_in_the_file() {
    let token = minted();
    let padding = "pad-zz ".repeat(700);
    assert!(padding.len() > 4096);
    let file = tempfile::tempfile().unwrap();
    let sink = file.try_clone().unwrap();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || RedactingWriter(sink.try_clone().unwrap()))
        .finish();
    let _log = crate::test_log::install(subscriber);
    tracing::error!(
        token = %token,
        "open http://127.0.0.1:1234/edit?secret=Ab12Cd34&id=1 {padding}"
    );
    let mut written = String::new();
    let mut file = file;
    std::io::Seek::rewind(&mut file).unwrap();
    file.read_to_string(&mut written).unwrap();
    assert!(!written.contains(&token), "the token reached the file");
    assert!(!written.contains("Ab12Cd34"), "the secret reached the file");
    assert_eq!(written.matches(MARK).count(), 2, "{written}");
    assert!(written.contains(&padding), "the padding was lost");
}

#[test]
fn only_a_run_of_exactly_32_lowercase_hex_characters_is_masked() {
    for n in [31usize, 33, 64] {
        let run = "a1".repeat(n).chars().take(n).collect::<String>();
        assert_eq!(red(&run), run, "{n} characters");
    }
    let run32: String = "0123456789abcdef".repeat(2);
    assert_eq!(red(&run32), MARK);
    assert_eq!(red(&format!("id={run32}.")), format!("id={MARK}."));
}

#[test]
fn uppercase_hex_and_other_letters_bound_a_run() {
    let run32: String = "0123456789abcdef".repeat(2);
    assert_eq!(red(&format!("A{run32}B")), format!("A{MARK}B"));
    assert_eq!(red(&format!("{run32}g")), format!("{MARK}g"));
    let upper = run32.to_uppercase();
    assert_eq!(red(&upper), upper, "uppercase hex is not in the class");
}

#[test]
fn the_value_after_secret_ends_at_each_delimiter() {
    for end in ["&id=1", "#frag", " tail", "\"tail", "'tail", "<tail", ">tail", ")tail", "\\tail", ""] {
        assert_eq!(red(&format!("a?secret=Ab12Cd34{end}")), format!("a?secret={MARK}{end}"), "{end:?}");
    }
}

#[test]
fn two_secrets_on_one_line_are_both_masked() {
    assert_eq!(red("x?secret=one&y?secret=two z"), format!("x?secret={MARK}&y?secret={MARK} z"));
}

#[test]
fn a_secret_marker_at_the_end_has_nothing_to_mask() {
    assert_eq!(red("tail secret="), "tail secret=");
    assert_eq!(red("secret=&x"), "secret=&x");
}

#[test]
fn no_match_returns_the_input_borrowed() {
    let input = b"plain text with id=12 and a short hex deadbeef";
    assert!(matches!(redact(input), Cow::Borrowed(_)));
    assert!(matches!(redact(b""), Cow::Borrowed(_)));
}

#[test]
fn one_write_call_masks_the_whole_buffer_and_reports_it_all_written() {
    use std::io::Write;
    let mut sink = Vec::new();
    let n = RedactingWriter(&mut sink).write(b"u?secret=abc\n").unwrap();
    assert_eq!(n, 13);
    assert_eq!(sink, format!("u?secret={MARK}\n").into_bytes());
}
